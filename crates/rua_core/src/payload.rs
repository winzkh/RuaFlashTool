use async_trait::async_trait;
use std::path::Path;
use std::sync::Arc;

#[async_trait]
pub trait ProgressReporter: Send + Sync {
    fn on_start(&self, name: &str, total: u64);
    fn on_progress(&self, name: &str, current: u64, total: u64);
    fn on_complete(&self, name: &str, total: u64);
    fn on_warning(&self, name: &str, idx: usize, msg: String);
    fn should_cancel(&self) -> bool { false }
}

#[derive(Debug)]
pub struct PayloadChunk {
    pub data: Vec<u8>,
    pub data_length: u64,
    pub output_path: String,
}

pub async fn unpack_payload(
    payload_path: &Path,
    output_dir: &Path,
    reporter: Arc<dyn ProgressReporter>,
) -> anyhow::Result<()> {
    use payload_dumper::extractor::local::{
        extract_partition, extract_partition_zip, list_partitions, list_partitions_zip,
        ExtractionProgress, ExtractionStatus, ProgressCallback,
    };

    let payload_path = payload_path.to_path_buf();
    let output_dir = output_dir.to_path_buf();
    let reporter_clone = reporter.clone();

    let res = std::thread::spawn(move || -> anyhow::Result<()> {
        let is_zip = payload_path
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.eq_ignore_ascii_case("zip"))
            .unwrap_or(false);

        let json = if is_zip { list_partitions_zip(&payload_path)? } else { list_partitions(&payload_path)? };

        let summary: payload_dumper::extractor::local::PayloadSummary =
            serde_json::from_str(&json)?;

        for p in summary.partitions {
            if reporter_clone.should_cancel() {
                return Err(anyhow::anyhow!("operation cancelled by user"));
            }
            let part_name = p.name;
            let out_path = output_dir.join(format!("{}.img", &part_name));
            let cb_reporter = reporter_clone.clone();
            let cb_part = part_name.clone();
            let total_bytes = p.size_bytes;
            let total_ops = p.operations_count as u64;
            let callback: ProgressCallback = Box::new(move |prog: ExtractionProgress| {
                match prog.status {
                    ExtractionStatus::Started => {
                        cb_reporter.on_start(&cb_part, total_bytes)
                    }
                    ExtractionStatus::InProgress => {
                        let current_ops = prog.current_operation.min(total_ops);
                        let current_bytes = if total_ops > 0 {
                            (current_ops as u128 * total_bytes as u128 / total_ops as u128) as u64
                        } else {
                            0
                        };
                        cb_reporter.on_progress(&cb_part, current_bytes, total_bytes)
                    }
                    ExtractionStatus::Completed => {
                        cb_reporter.on_complete(&cb_part, total_bytes)
                    }
                    ExtractionStatus::Warning {
                        operation_index,
                        message,
                    } => cb_reporter.on_warning(&cb_part, operation_index, message),
                }
                !cb_reporter.should_cancel()
            });

            if is_zip {
                extract_partition_zip(
                    &payload_path,
                    &part_name,
                    &out_path,
                    Some(callback),
                    Option::<&std::path::Path>::None,
                )?;
            } else {
                extract_partition(
                    &payload_path,
                    &part_name,
                    &out_path,
                    Some(callback),
                    Option::<&std::path::Path>::None,
                )?;
            }
        }
        Ok(())
    })
    .join()
    .map_err(|e| anyhow::anyhow!("payload extraction thread panicked: {:?}", e))?;

    res
}

pub async fn extract_single_partition(
    payload_path: &Path,
    partition: &str,
    output_dir: &Path,
    reporter: Arc<dyn ProgressReporter>,
) -> anyhow::Result<std::path::PathBuf> {
    use payload_dumper::extractor::local::{
        extract_partition, extract_partition_zip, list_partitions, list_partitions_zip,
        ExtractionProgress, ExtractionStatus, ProgressCallback,
    };
    let payload_path = payload_path.to_path_buf();
    let output_dir = output_dir.to_path_buf();
    let partition_name = partition.to_string();
    let reporter_clone = reporter.clone();

    let out = std::thread::spawn(move || -> anyhow::Result<std::path::PathBuf> {
        let is_zip = payload_path
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.eq_ignore_ascii_case("zip"))
            .unwrap_or(false);

        let json = if is_zip { list_partitions_zip(&payload_path)? } else { list_partitions(&payload_path)? };
        let summary: payload_dumper::extractor::local::PayloadSummary = serde_json::from_str(&json)?;

        let p = summary.partitions.into_iter().find(|p| p.name == partition_name)
            .ok_or_else(|| anyhow::anyhow!("未在 payload 中找到分区: {}", partition_name))?;

        if reporter_clone.should_cancel() {
            return Err(anyhow::anyhow!("operation cancelled by user"));
        }

        let total_bytes = p.size_bytes;
        let total_ops = p.operations_count as u64;
        let cb_reporter = reporter_clone.clone();
        let cb_part = partition_name.clone();
        let out_path = output_dir.join(format!("{}.img", &cb_part));

        let callback: ProgressCallback = Box::new(move |prog: ExtractionProgress| {
            match prog.status {
                ExtractionStatus::Started => {
                    cb_reporter.on_start(&cb_part, total_bytes)
                }
                ExtractionStatus::InProgress => {
                    let current_ops = prog.current_operation.min(total_ops);
                    let current_bytes = if total_ops > 0 {
                        (current_ops as u128 * total_bytes as u128 / total_ops as u128) as u64
                    } else { 0 };
                    cb_reporter.on_progress(&cb_part, current_bytes, total_bytes)
                }
                ExtractionStatus::Completed => {
                    cb_reporter.on_complete(&cb_part, total_bytes)
                }
                ExtractionStatus::Warning { operation_index, message } => {
                    cb_reporter.on_warning(&cb_part, operation_index, message)
                }
            }
            !cb_reporter.should_cancel()
        });

        if is_zip {
            extract_partition_zip(&payload_path, &partition_name, &out_path, Some(callback), Option::<&std::path::Path>::None)?;
        } else {
            extract_partition(&payload_path, &partition_name, &out_path, Some(callback), Option::<&std::path::Path>::None)?;
        }
        Ok(out_path)
    })
    .join()
    .map_err(|e| anyhow::anyhow!("payload extraction thread panicked: {:?}", e))?;

    out
}
