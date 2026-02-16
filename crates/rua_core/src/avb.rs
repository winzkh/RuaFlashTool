use crate::error::{FlashError, Result};
use num_bigint::BigUint;
use rsa::pkcs1::DecodeRsaPrivateKey;
use rsa::pkcs8::DecodePrivateKey;
use rsa::traits::PublicKeyParts;
use rsa::RsaPrivateKey;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::path::Path;

const FOOTER_SIZE: usize = 64;
const VBMETA_HEADER_SIZE: usize = 256;
const AVB_MAGIC: &[u8; 4] = b"AVB0";
const AVB_FOOTER_MAGIC: &[u8; 4] = b"AVBf";

fn align_up(x: usize, a: usize) -> usize {
    (x + a - 1) / a * a
}

fn be32(v: u32) -> [u8; 4] {
    v.to_be_bytes()
}
fn be64(v: u64) -> [u8; 8] {
    v.to_be_bytes()
}

fn build_public_key_blob(priv_key: &RsaPrivateKey) -> Vec<u8> {
    let n = BigUint::from_bytes_be(&priv_key.n().to_bytes_be());
    let bits = n.bits() as u32;
    let key_bytes = (bits as usize + 7) / 8;

    let n_be = n.to_bytes_be();
    let n0 = if n_be.is_empty() {
        0u32
    } else {
        let mut tail = [0u8; 4];
        for i in 0..4 {
            let idx = n_be.len().saturating_sub(4 - i);
            if idx > 0 && idx - 1 < n_be.len() {
                tail[i] = n_be[n_be.len() - 4 + i];
            }
        }
        u32::from_be_bytes(tail)
    };
    let modulus = 0x1_0000_0000u64;
    fn egcd(a: i64, b: i64) -> (i64, i64, i64) {
        if a == 0 {
            (b, 0, 1)
        } else {
            let (g, x, y) = egcd(b % a, a);
            (g, y - (b / a) * x, x)
        }
    }
    let mut inv = 0u32;
    if n0 != 0 {
        let a = n0 as i64;
        let m = modulus as i64;
        let (g, x, _) = egcd(a.rem_euclid(m), m);
        if g == 1 {
            let xi = (x % m + m) % m;
            let ninv = xi as u64;
            let n0inv = ((modulus - ninv) % modulus) as u32;
            inv = n0inv;
        }
    }
    let one = BigUint::from(1u32);
    let r = &one << bits;
    let rr = (&r * &r) % &n;
    let rr_be = rr.to_bytes_be();

    let mut out = Vec::with_capacity(8 + key_bytes * 2);
    out.extend_from_slice(&be32(bits));
    out.extend_from_slice(&be32(inv));
    if n_be.len() < key_bytes {
        out.resize(out.len() + (key_bytes - n_be.len()), 0);
    }
    out.extend_from_slice(&n_be[n_be.len().saturating_sub(key_bytes)..]);
    if rr_be.len() < key_bytes {
        out.resize(out.len() + (key_bytes - rr_be.len()), 0);
    }
    out.extend_from_slice(&rr_be[rr_be.len().saturating_sub(key_bytes)..]);
    out
}

fn build_hash_descriptor(
    partition_name: &str,
    image_data: &[u8],
) -> (Vec<u8>, Vec<u8>) {
    let mut hasher = Sha256::new();
    hasher.update(image_data);
    let digest = hasher.finalize().to_vec();

    let partition_name_bytes = partition_name.as_bytes();
    let name_len = partition_name_bytes.len() as u32;
    let salt_len = 0u32;
    let digest_len = digest.len() as u32;

    let parent_size = 16usize;
    let hash_desc_fixed_size = 8 + 32 + 4 + 4 + 4 + 4 + 60;
    let mut num_following = (hash_desc_fixed_size + name_len as usize + salt_len as usize + digest_len as usize) as u64;
    if num_following % 8 != 0 {
        num_following += 8 - (num_following % 8);
    }

    let mut desc = Vec::with_capacity(parent_size + num_following as usize);
    desc.extend_from_slice(&be64(2));
    desc.extend_from_slice(&be64(num_following));
    desc.extend_from_slice(&be64(image_data.len() as u64));
    let mut algo = [0u8; 32];
    let s = b"sha256";
    algo[..s.len()].copy_from_slice(s);
    desc.extend_from_slice(&algo);
    desc.extend_from_slice(&be32(name_len));
    desc.extend_from_slice(&be32(salt_len));
    desc.extend_from_slice(&be32(digest_len));
    desc.extend_from_slice(&be32(0));
    desc.extend_from_slice(&[0u8; 60]);
    desc.extend_from_slice(partition_name_bytes);
    desc.extend_from_slice(&digest);
    while desc.len() % 8 != 0 {
        desc.push(0);
    }
    (desc, digest)
}

pub async fn add_hash_footer(
    image_path: &str,
    partition_name: &str,
    partition_size_bytes: u64,
    key_pem_path: &str,
    algorithm: &str,
) -> Result<String> {
    let image = fs::read(image_path)
        .map_err(|e| FlashError::PatchError(format!("read image failed: {:?}", e)))?;
    let orig_size = image.len() as u64;
    if orig_size > partition_size_bytes {
        return Err(FlashError::PatchError(
            "image larger than partition size".to_string(),
        ));
    }
    let pem_txt = fs::read_to_string(key_pem_path)
        .map_err(|e| FlashError::PatchError(format!("read key failed: {:?}", e)))?;
    if pem_txt.to_lowercase().contains("begin public key") {
        return Err(FlashError::PatchError(
            "invalid key: public key not allowed".to_string(),
        ));
    }
    let priv_key = RsaPrivateKey::from_pkcs1_pem(&pem_txt)
        .or_else(|_| RsaPrivateKey::from_pkcs8_pem(&pem_txt))
        .map_err(|e| FlashError::PatchError(format!("parse rsa key failed: {:?}", e)))?;

    let pubkey_blob = build_public_key_blob(&priv_key);
    let (hash_desc, _digest) = build_hash_descriptor(partition_name, &image);

    let pubkey_offset = 0u64;
    let pubkey_size = pubkey_blob.len() as u64;
    let descriptors_offset = align_up(pubkey_blob.len(), 8) as u64;
    let desc_size = hash_desc.len() as u64;

    let mut aux = Vec::with_capacity(align_up(
        (descriptors_offset + desc_size) as usize,
        64,
    ));
    aux.extend_from_slice(&pubkey_blob);
    while aux.len() < descriptors_offset as usize {
        aux.push(0);
    }
    aux.extend_from_slice(&hash_desc);
    while aux.len() % 64 != 0 {
        aux.push(0);
    }
    let aux_size = aux.len() as u64;

    let (algo_type, sig_len) = match algorithm {
        "SHA256_RSA4096" => (2u32, 512usize),
        _ => (1u32, 256usize),
    };
    let hash_len = 32usize;

    let authentication_data_block_size = align_up(hash_len + sig_len, 64) as u64;
    let auxiliary_data_block_size = aux_size;
    let hash_offset = 0u64;
    let hash_size = hash_len as u64;
    let signature_offset = hash_size;
    let signature_size = sig_len as u64;
    let public_key_offset = pubkey_offset;
    let public_key_size = pubkey_size;
    let public_key_metadata_offset = 0u64;
    let public_key_metadata_size = 0u64;
    let descriptors_off = descriptors_offset;
    let descriptors_size = desc_size;
    let rollback_index = 0u64;
    let flags = 0u32;
    let release_string = b"rua_avb 1.0\0";

    let mut header = vec![0u8; VBMETA_HEADER_SIZE];
    header[0..4].copy_from_slice(AVB_MAGIC);
    header[4..8].copy_from_slice(&be32(1));
    header[8..12].copy_from_slice(&be32(0));
    header[12..20].copy_from_slice(&be64(authentication_data_block_size));
    header[20..28].copy_from_slice(&be64(auxiliary_data_block_size));
    header[28..32].copy_from_slice(&be32(algo_type));
    header[32..40].copy_from_slice(&be64(hash_offset));
    header[40..48].copy_from_slice(&be64(hash_size));
    header[48..56].copy_from_slice(&be64(signature_offset));
    header[56..64].copy_from_slice(&be64(signature_size));
    header[64..72].copy_from_slice(&be64(public_key_offset));
    header[72..80].copy_from_slice(&be64(public_key_size));
    header[80..88].copy_from_slice(&be64(public_key_metadata_offset));
    header[88..96].copy_from_slice(&be64(public_key_metadata_size));
    header[96..104].copy_from_slice(&be64(descriptors_off));
    header[104..112].copy_from_slice(&be64(descriptors_size));
    header[112..120].copy_from_slice(&be64(rollback_index));
    header[120..124].copy_from_slice(&be32(flags));
    header[128..128 + release_string.len()].copy_from_slice(release_string);

    let mut hasher = Sha256::new();
    hasher.update(&header);
    hasher.update(&aux);
    let vbmeta_digest = hasher.finalize().to_vec();

    use rsa::signature::{RandomizedSigner, SignatureEncoding};
    use rsa::pkcs1v15::SigningKey;
    use rand::rngs::OsRng;
    let signing_key = SigningKey::<Sha256>::new(priv_key);
    let mut rng = OsRng;
    let mut sign_input = Vec::with_capacity(header.len() + aux.len());
    sign_input.extend_from_slice(&header);
    sign_input.extend_from_slice(&aux);
    let signature = signing_key.sign_with_rng(&mut rng, &sign_input);
    let signature_bytes = signature.to_bytes().to_vec();
    if signature_bytes.len() != sig_len {
        return Err(FlashError::PatchError(
            "signature length mismatch".to_string(),
        ));
    }

    let mut auth = Vec::with_capacity(align_up(hash_len + sig_len, 64));
    auth.extend_from_slice(&vbmeta_digest);
    auth.extend_from_slice(&signature_bytes);
    while auth.len() % 64 != 0 {
        auth.push(0);
    }

    let vbmeta = {
        let mut v = Vec::with_capacity(header.len() + auth.len() + aux.len());
        v.extend_from_slice(&header);
        v.extend_from_slice(&auth);
        v.extend_from_slice(&aux);
        v
    };
    let vbmeta_size = vbmeta.len() as u64;

    let total = orig_size + vbmeta_size + FOOTER_SIZE as u64;
    if total > partition_size_bytes {
        return Err(FlashError::PatchError(
            "signed image would exceed partition size".to_string(),
        ));
    }

    let vbmeta_offset = orig_size;
    let mut footer = vec![0u8; FOOTER_SIZE];
    footer[0..4].copy_from_slice(AVB_FOOTER_MAGIC);
    footer[4..8].copy_from_slice(&be32(1));
    footer[8..12].copy_from_slice(&be32(0));
    footer[12..20].copy_from_slice(&be64(orig_size));
    footer[20..28].copy_from_slice(&be64(vbmeta_offset));
    footer[28..36].copy_from_slice(&be64(vbmeta_size));

    let out_path = format!(
        "{}.signed.img",
        Path::new(image_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("patched")
    );
    let mut f = fs::File::create(&out_path)
        .map_err(|e| FlashError::PatchError(format!("create out failed: {:?}", e)))?;
    f.write_all(&image)
        .map_err(|e| FlashError::PatchError(format!("write image failed: {:?}", e)))?;
    f.write_all(&vbmeta)
        .map_err(|e| FlashError::PatchError(format!("write vbmeta failed: {:?}", e)))?;
    f.write_all(&footer)
        .map_err(|e| FlashError::PatchError(format!("write footer failed: {:?}", e)))?;
    Ok(out_path)
}
