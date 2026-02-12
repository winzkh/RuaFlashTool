use thiserror::Error;

#[derive(Error, Debug)]
pub enum FlashError {
    #[error("未找到设备，请检查 USB 连接")]
    DeviceNotFound,

    #[error("未找到 fastboot 可执行文件，期待路径: {0}")]
    FastbootExecutableNotFound(String),

    #[error("未找到 adb 可执行文件，期待路径: {0}")]
    AdbExecutableNotFound(String),

    #[error("Fastboot 错误: {0}")]
    FastbootError(String),

    #[error("ADB 错误: {0}")]
    AdbError(String),

    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),

    #[error("解包失败: {0}")]
    UnpackError(String),

    #[error("LZ4 错误: {0}")]
    Lz4Error(#[from] lz4_flex::frame::Error),

    #[error("修补错误: {0}")]
    PatchError(String),

    #[error("无效的选择: {0}")]
    InvalidChoice(String),

    #[error("操作已由用户取消")]
    Interrupted,

    #[error("操作已取消")]
    Cancelled,

    #[error("属性未找到: {0}")]
    PropertyNotFound(String),

    #[error("其他错误: {0}")]
    Anyhow(#[from] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, FlashError>;
