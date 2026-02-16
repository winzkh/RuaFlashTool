pub const APP_NAME: &str = "RuaFlashTool";
pub const VERSION: &str = "1.0.0-rc2";
pub const AUTHOR: &str = "酷安@射光灯";

pub const QQ_GROUPS: &[&str] = &[
    "1080486382",
    "1080474851",
];

pub const WARNING_TEXTS: &[&str] = &[
    "⚠️  刷机不规范，救砖两行泪",
    "⚠️  请务必备份重要数据，本工具不对任何变砖风险负责",
];

pub const INFO_TEXTS: &[&str] = &[
    "💡  KernelSU处包括原版KernelSU及其分支版本KernelSU Next、SukiSU Ultra以及KowSU",
    "💡  请务必备份重要数据，本工具不对任何变砖风险负责",
];

pub const MENU_OPTIONS: &[(&str, &str)] = &[
    ("1", "Fastboot一键刷入线刷包（小米线刷包专用）"),
    ("2", "Fastboot一键刷入卡刷包（适用卡刷包）"),
    ("3", "Fastboot一键刷入目录下全部分区"),
    ("4", "FastbootD一键刷入目录下全部分区"),
    ("5", "通用 Bootloader Lock 状态管理 (解锁/回锁)"),
    ("6", "下载小米解锁工具"),
    ("7", "一键Root刷入Magisk（含Alpha/Kitsune分支）"),
    ("8", "一键Root刷入APatch（兼容ForkPatch）"),
    ("9", "一键Root刷入KernelSU LKM模式（需内核版本≥5.10）"),
    ("10", "一键Root自选AnyKernel3刷入（刷入KSU内核常用）"),
    ("11", "自定义选择分区刷入"),
    ("12", "一键安装刷机驱动"),
    ("13", "一键关闭 AVB (刷入 vbmeta 并禁用校验)"),
    ("14", "打开命令行窗口"),
    ("15", "检测设备连接状态"),
    ("16", "ADB投屏"),
    ("17", "一键安装 APK"),
    ("18", "Fastboot(D)恢复出厂设置"),
    ("19", "重启设备 (系统/Recovery/FastbootD/Bootloader/EDL)"),
    ("20", "切换槽位 (极其危险)"),
    ("21", "ADB 激活 (Shizuku/冰箱/黑阈等)"),
    ("22", "打开设备管理器"),
    ("0", "退出程序"),
];
