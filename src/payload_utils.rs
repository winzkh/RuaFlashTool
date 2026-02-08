use std::path::Path;
use colored::Colorize;
use std::sync::Arc;
use anyhow::Result;
use payload_dumper::payload::payload_dumper::{dump_partition, ProgressReporter, AsyncPayloadRead};
use payload_dumper::payload::payload_parser::{parse_local_payload, parse_local_zip_payload};
use payload_dumper::readers::local_reader::LocalAsyncPayloadReader;
use payload_dumper::readers::local_zip_reader::LocalAsyncZipPayloadReader;
use std::fs;
use std::time::Instant;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use async_trait::async_trait;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use dashmap::DashMap;

pub struct IndicatifReporter {
    multi: MultiProgress,
    bars: DashMap<String, ProgressBar>,
    overall: ProgressBar,
}

impl IndicatifReporter {
    pub fn new(multi: MultiProgress, total_overall_ops: u64) -> Self {
        let overall = multi.add(ProgressBar::new(total_overall_ops));
        overall.set_style(
            ProgressStyle::with_template(
                "{prefix:>15.bold.magenta} {spinner:.magenta} [{bar:40.magenta/white}] {percent:>3}% ({pos}/{len} ops, ETA: {eta:<4})"
            )
            .unwrap()
            .progress_chars("=>-")
        );
        overall.set_prefix("TOTAL");
        
        Self {
            multi,
            bars: DashMap::new(),
            overall,
        }
    }
}

#[async_trait]
impl ProgressReporter for IndicatifReporter {
    fn on_start(&self, partition_name: &str, total_operations: u64) {
        let pb = self.multi.add(ProgressBar::new(total_operations));
        
        let style = ProgressStyle::with_template(
            "{msg:<15.bold.cyan} {spinner:.green} [{bar:30.blue/white}] {percent:>3}% ({pos}/{len} ops, ETA: {eta:<4})"
        )
        .unwrap()
        .progress_chars("=>-");
        
        pb.set_style(style);
        pb.set_message(partition_name.to_string());
        self.bars.insert(partition_name.to_string(), pb);
    }

    fn on_progress(&self, partition_name: &str, current_op: u64, _total_ops: u64) {
        if let Some(pb) = self.bars.get(partition_name) {
            pb.set_position(current_op);
        }
        self.overall.inc(1);
    }

    fn on_complete(&self, partition_name: &str, _total_operations: u64) {
        if let Some(kv) = self.bars.get(partition_name) {
            let pb = kv.value();
            pb.set_style(ProgressStyle::with_template("{msg:<15.bold.green} ✔  Done ({elapsed})").unwrap());
            pb.finish_with_message(partition_name.to_string());
        }
    }

    fn on_warning(&self, partition_name: &str, _operation_index: usize, message: String) {
        self.multi.println(format!(">> [警告] {}: {}", partition_name.yellow(), message)).unwrap();
    }
}

pub async fn unpack_payload(input: &Path, output: &Path) -> Result<()> {
    let start_time = Instant::now();
    let extension = input.extension().and_then(|e| e.to_str()).unwrap_or("");
    let is_zip = extension.eq_ignore_ascii_case("zip");

    println!("{}", "============================================================".white());
    println!("{}", ">> [性能模式] 启动高速解包引擎...".bright_magenta().bold());

    // 1. 获取 Reader 和 Manifest
    let (manifest, data_offset, payload_reader): (_, _, Arc<dyn AsyncPayloadRead>) = if is_zip {
        println!(">> [引擎] 正在使用零拷贝技术映射 ZIP 内部的 payload.bin...");
        let (manifest, offset) = parse_local_zip_payload(input.to_path_buf()).await?;
        let zip_reader = LocalAsyncZipPayloadReader::new(input.to_path_buf()).await?;
        (manifest, offset, Arc::new(zip_reader))
    } else {
        println!(">> [引擎] 正在建立 payload.bin 高速映射...");
        let (manifest, offset) = parse_local_payload(input).await?;
        let bin_reader = LocalAsyncPayloadReader::new(input.to_path_buf()).await?;
        (manifest, offset, Arc::new(bin_reader))
    };

    if !output.exists() {
        fs::create_dir_all(output)?;
    }

    let block_size = manifest.block_size.unwrap_or(4096) as u64;
    let partitions = manifest.partitions;
    let partitions_count = partitions.len();
    
    // 计算总块数用于总进度条
    let total_ops: u64 = partitions.iter().map(|p| p.operations.len() as u64).sum();

    // 2. 并行配置
    let concurrency = num_cpus::get().min(8); 
    println!(">> [配置] 开启并行提取 (并发数: {})", concurrency);
    println!(">> [配置] 发现分区总数: {}", partitions_count);
    println!("{}", "------------------------------------------------------------".white());

    let semaphore = Arc::new(Semaphore::new(concurrency));
    let mut join_set = JoinSet::new();
    
    let multi = MultiProgress::new();
    let reporter = Arc::new(IndicatifReporter::new(multi.clone(), total_ops));

    for partition in partitions {
        let partition = partition.clone();
        let payload_reader = Arc::clone(&payload_reader);
        let output_path = output.join(format!("{}.img", partition.partition_name));
        let sem = Arc::clone(&semaphore);
        let rep = Arc::clone(&reporter);

        join_set.spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            dump_partition(
                &partition,
                data_offset,
                block_size,
                output_path,
                &payload_reader,
                rep.as_ref(),
                None,
            ).await
        });
    }

    // 在后台运行渲染
    // let render_task = tokio::task::spawn_blocking(move || {
    //     // MultiProgress handles its own rendering
    // });

    // 3. 等待所有任务完成
    let mut finished = 0;
    let mut _failed = false;

    while finished < partitions_count {
        if crate::INTERRUPTED.load(std::sync::atomic::Ordering::SeqCst) {
            multi.println(format!("{}", ">> [中断] 正在取消剩余任务...".yellow())).unwrap();
            join_set.abort_all();
            break;
        }

        tokio::select! {
            Some(res) = join_set.join_next() => {
                match res {
                    Ok(Ok(_)) => {
                        finished += 1;
                    }
                    Ok(Err(e)) => {
                        multi.println(format!("\n>> [错误] 某个分区提取失败: {:?}", e)).unwrap();
                        _failed = true;
                        finished += 1;
                    }
                    Err(e) if e.is_panic() => {
                        multi.println(format!("\n>> [任务异常] 提取线程崩溃: {:?}", e)).unwrap();
                        finished += 1;
                    }
                    Err(_) => {
                        finished += 1;
                    }
                }
            }
            _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {}
        }
    }

    reporter.overall.finish_with_message("All Done");
    let duration = start_time.elapsed();
    
    // 计算总大小
    let mut total_size = 0u64;
    if let Ok(entries) = fs::read_dir(output) {
        for entry in entries.flatten() {
            if entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                total_size += entry.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    
    let total_mb = total_size as f32 / 1024.0 / 1024.0;
    let avg_speed = total_mb / duration.as_secs_f32();

    println!("{}", "------------------------------------------------------------".white());
    println!(
        ">> [总结] 极速提取完成！耗时: {:.2}s, 总大小: {:.2} MB, 平均吞吐量: {:.2} MB/s",
        duration.as_secs_f32(), total_mb, avg_speed
    );
    println!("{}", ">> [提示] 镜像已存放至 extracted_images 目录。".bright_yellow());
    println!("{}", "============================================================".white());
    
    Ok(())
}
