use crate::error::{FlashError, Result};
use android_bootimg::{parser::BootImage, patcher::BootImagePatchOption};
use std::io::Cursor;

pub fn new_patcher<'a>(boot_img: &'a BootImage) -> BootImagePatchOption<'a> {
    BootImagePatchOption::new(boot_img)
}

pub fn patch_to_vec(patcher: BootImagePatchOption) -> Result<Vec<u8>> {
    let mut out = Cursor::new(Vec::new());
    patcher
        .patch(&mut out)
        .map_err(|e| FlashError::PatchError(e.to_string()))?;
    Ok(out.into_inner())
}

pub fn patch_with_replacements(
    boot_img: &BootImage,
    kernel: Option<(Vec<u8>, bool)>,
    ramdisk: Option<(Vec<u8>, bool)>,
) -> Result<Vec<u8>> {
    let mut patcher = new_patcher(boot_img);
    if let Some((kbytes, flag)) = kernel {
        patcher.replace_kernel(Box::new(Cursor::new(kbytes)), flag);
    }
    if let Some((rbytes, preserve_all)) = ramdisk {
        patcher.replace_ramdisk(Box::new(Cursor::new(rbytes)), preserve_all);
    }
    patch_to_vec(patcher)
}
