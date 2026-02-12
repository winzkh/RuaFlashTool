use async_trait::async_trait;
use std::path::Path;
use std::sync::Arc;
use tokio::fs::File;

#[async_trait]
pub trait ProgressReporter: Send + Sync {
    fn on_start(&self, name: &str, total: u64);
    fn on_progress(&self, name: &str, current: u64, total: u64);
    fn on_complete(&self, name: &str, total: u64);
    fn on_warning(&self, name: &str, idx: usize, msg: String);
}

#[derive(Debug)]
pub struct PayloadChunk {
    pub data: Vec<u8>,
    pub data_length: u64,
    pub output_path: String,
}

pub async fn unpack_payload(
    payload_path: &Path,
    _output_dir: &Path,
    reporter: Arc<dyn ProgressReporter>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    reporter.on_start("Payload", 0);
    
    let file = File::open(payload_path).await?;
    let metadata = file.metadata().await?;
    let total_size = metadata.len();
    
    reporter.on_complete("Payload", total_size);
    Ok(())
}
