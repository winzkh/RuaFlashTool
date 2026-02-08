pub const APP_NAME: &str = "RuaFlashTool";
pub const VERSION: &str = "0.0.2";
pub const AUTHOR: &str = "酷安@射光灯";

pub const QQ_GROUPS: &[&str] = &[
    "1080486382",
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
    ("2", "Fastboot一键刷入卡刷包（适用小米、一加卡刷包）"),
    ("3", "Fastboot一键刷入目录下全部分区（需不包括逻辑分区）"),
    ("4", "FastbootD模式一键刷机（请确保Recovery正常，支持逻辑分区）"),
    ("5", "一键安装刷机驱动"),
    ("6", "通用一键解锁Bootloader"),
    ("7", "回锁 Bootloader (谨慎操作)"),
    ("8", "下载小米解锁工具"),
    ("9", "一键Root刷入Magisk"),
    ("10", "一键Root刷入Magisk Alpha"),
    ("11", "一键Root刷入Kitsune Mask (原Magisk Delta)"),
    ("12", "一键Root刷入APatch（同时兼容ForkPatch）"),
    ("13", "一键Root刷入KernelSU LKM模式（需要内核版本号≥5.10）"),
    ("14", "一键Root自选AnyKernel3刷入（刷入KernelSU内核常用）"),
    ("15", "一键刷入Boot"),
    ("16", "一键关闭 AVB (刷入 vbmeta 并禁用校验)"),
    ("17", "自定义选择分区刷入"),
    ("18", "打开命令行窗口"),
    ("19", "检测设备连接状态"),
    ("20", "ADB投屏"),
    ("21", "一键安装 APK"),
    ("22", "Fastboot(D)恢复出厂设置 (清除 Data/Cache)"),
    ("23", "重启到系统"),
    ("24", "重启到 Recovery"),
    ("25", "重启到 FastbootD"),
    ("26", "重启到 Bootloader (Fastboot)"),
    ("27", "重启到 9008 (EDL)/深刷模式"),
    ("28", "切换活动槽位 (极其危险)"),
    ("0", "退出程序"),
];

