//! 沙盒执行的"意图"描述。
//!
//! 这一层刻意与平台无关：它只描述"我想跑什么、允许它碰什么"，
//! 至于用什么机制实现（bwrap / sandbox-exec / 直接进程），交给 backend。

// `use` 是"引入外部类型"，避免每次写全路径。
// PathBuf：一个"文件路径"类型，能表示 /Users/xxx/a.txt 这种。
use std::path::PathBuf;
// Duration：一个"时间段"类型，比如 60 秒、200 毫秒。
use std::time::Duration;

/// 网络出口策略。默认是 `Disabled` —— 对应"假世界默认无网"。
///
/// `enum` 是"多选一"的类型：一个值只能是下面几种之一。
#[derive(Debug, Clone, Default)] // Debug=能打印；Clone=能复制；Default=能取默认值
pub enum NetworkPolicy {
    /// 完全无网。backend 必须保证进程无法建立任何连接。
    #[default] // 标记"默认就是它"——所以 NetworkPolicy::default() 得到 Disabled
    Disabled,

    /// 只允许经由指定代理出网（如 `127.0.0.1:8888`）。
    /// 真实实现里这是唯一被放行的出口，代理负责域名白名单与审计。
    Proxy {
        // 花括号写法表示这个变体还带数据：一个地址字符串。
        // 比如 NetworkPolicy::Proxy { addr: "127.0.0.1:8888".into() }
        addr: String,
    },
}

/// 一次命令执行的完整规格。
///
/// 注意：这里没有"命令字符串"。program 与 args 是分离的，
/// 避免 `bash -c` 那种"把语义藏在字符串里、无法静态判断"的问题。
#[derive(Debug, Clone)] // 可打印、可复制（注意没有 Default，下面手写了）
pub struct SandboxSpec {
    // `struct` 是"打包一组字段"的类型。`pub` 表示外面能直接读。

    /// 要执行的程序，例如 `/usr/bin/python3`。
    pub program: String, // String：可变长字符串

    /// 参数列表，逐项传递，不做字符串拆分。
    pub args: Vec<String>, // Vec<T>：一串 T，这里是一串字符串

    /// 工作目录。默认取 `workspace_root`。
    pub cwd: Option<PathBuf>, // Option<T>：要么有值 Some(T)，要么没有 None

    /// 注入的环境变量。未列出的环境变量默认不继承（避免泄漏 API key）。
    pub env: Vec<(String, String)>, // 一串"键值对"，用元组 (String, String) 表示

    /// 墙钟超时。超时由外层强制 kill，不依赖进程自觉退出。
    pub timeout: Duration, // 一个时间段，比如 60 秒

    /// 工作区根目录。所有相对路径以此为界，backend 应据此限制 cwd。
    pub workspace_root: Option<PathBuf>,

    /// 允许写入的路径（相对或绝对）。其余一律只读或不可见。
    pub writable_paths: Vec<PathBuf>,

    /// 允许读取的路径。为空时 backend 采用自身默认只读集合。
    pub read_only_paths: Vec<PathBuf>,

    /// 网络策略。
    pub network: NetworkPolicy,

    /// 内存上限（MiB）。backend 视能力实现，不支持的应记录降级。
    pub memory_limit_mb: Option<u64>, // u64：无符号整数（不能是负数）

    /// 最大进程数（防 fork bomb）。
    pub max_processes: Option<u32>, // u32：无符号 32 位整数
}

// `impl Default for X` 表示"给 X 定义默认值"。
// 有了它，就能写 SandboxSpec::default() 得到一个"每个字段都是默认"的实例。
impl Default for SandboxSpec {
    // fn default() 是 Default trait 要求必须实现的函数，返回 Self（即 SandboxSpec）
    fn default() -> Self {
        Self {
            // Self { ... } 是"构造一个实例"，每个字段都要给值。
            program: String::new(),              // 空字符串 ""
            args: Vec::new(),                    // 空列表 []
            cwd: None,                           // 没有工作目录
            env: Vec::new(),                     // 空列表
            timeout: Duration::from_secs(60),    // 60 秒
            workspace_root: None,
            writable_paths: Vec::new(),
            read_only_paths: Vec::new(),
            network: NetworkPolicy::default(),   // 走上面 #[default] 的 Disabled
            memory_limit_mb: None,
            max_processes: None,
        }
    }
}

// `impl SandboxSpec` 表示"给 SandboxSpec 添加方法"。
// 这里的方法都是"链式"的：每个返回 Self，所以能一直 .arg().env().timeout() 串下去。
impl SandboxSpec {
    // `impl Into<String>` 是"任何能转成 String 的东西都行"，
    // 所以既能传 &str（"python3"），也能传 String。这是 Rust 惯用写法。
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(), // .into() 把传入的东西转成 String
            ..Default::default()     // `..` 是"其余字段用默认值填"（来自上面的 Default）
        }
    }

    // `mut self` 表示"接收所有权并允许修改"，改完再返回（链式风格）。
    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into()); // push：往列表末尾加一个
        self                        // 返回自己，好继续 .xxx()
    }

    // 泛型：I 是"任何可迭代的东西"，S 是"迭代出来的元素"。
    // where 后面是约束：I 能产出一串 S，S 能转成 String。
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        // extend：把一串东西全部追加进来。.map(Into::into) 把每个 S 转成 String。
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    // 一次加一个环境变量键值对。
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into())); // 打包成元组 (key, value) 放进列表
        self
    }

    // 设置工作目录。
    pub fn cwd(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cwd = Some(dir.into()); // 包成 Some(...)，表示"有值了"
        self
    }

    // 设置工作区根目录。
    pub fn workspace_root(mut self, dir: impl Into<PathBuf>) -> Self {
        self.workspace_root = Some(dir.into());
        self
    }

    // 追加一个可写路径。
    pub fn writable(mut self, path: impl Into<PathBuf>) -> Self {
        self.writable_paths.push(path.into());
        self
    }

    // 设置超时。
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout; // 直接覆盖
        self
    }

    // 设置网络策略。
    pub fn network(mut self, policy: NetworkPolicy) -> Self {
        self.network = policy;
        self
    }
}
