//! The configuration explained: one list in code of every `config.toml` field with what it
//! does, its kind, its allowed values and whether it applies at once or after a restart
//! (design, "M10": settings made clear). The console renders it, `docs/configuration.md` is
//! generated from it, and a test fails when a field of [`Config`] has no entry here or an entry
//! names no field. Defaults come from `Config::default()`, so they can't drift either.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

use crate::config::Config;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case", tag = "kind", content = "options")]
pub enum FieldKind {
    Text,
    /// Text that may be left unset.
    OptionalText,
    Integer,
    Boolean,
    /// One of these values.
    Choice(&'static [&'static str]),
    /// A file or folder, relative to the config file's folder.
    Path,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum Applies {
    /// Changing it takes effect at once.
    Now,
    /// It is read when the service starts.
    Restart,
}

pub struct Field {
    /// `section.key`, or `section.table.key` for a field of an inline table.
    pub path: &'static str,
    pub description: &'static str,
    pub kind: FieldKind,
    pub applies: Applies,
    /// For experiments and tests; the console folds these away.
    pub advanced: bool,
}

const fn f(path: &'static str, kind: FieldKind, description: &'static str) -> Field {
    Field {
        path,
        description,
        kind,
        applies: Applies::Restart,
        advanced: false,
    }
}

const fn advanced(path: &'static str, kind: FieldKind, description: &'static str) -> Field {
    Field {
        path,
        description,
        kind,
        applies: Applies::Restart,
        advanced: true,
    }
}

use FieldKind::*;

/// The sections, in the order the file and the console show them.
pub const SECTIONS: &[(&str, &str)] = &[
    (
        "server",
        "Where the service listens and how much it computes at once.",
    ),
    ("data", "Where the service keeps its data."),
    (
        "identity",
        "Who callers are: API keys or JWTs, and the headers that carry users, groups and audiences.",
    ),
    ("store", "Each tenant's SQLite file."),
    (
        "librarian",
        "The librarian, the one LLM the service calls (writes only). Which model and its key are in the model file.",
    ),
    (
        "converter",
        "Converting files. The service reads Markdown, text, HTML, Word, spreadsheets, PowerPoint and PDF itself; the optional MarkItDown converter reads the old binary Office formats.",
    ),
    (
        "search",
        "How searches rank and how many hits each slot holds.",
    ),
    ("trace", "Search traces, kept for replays."),
    (
        "embedding",
        "Meaning-based search: a local embedding model, off by default.",
    ),
    (
        "graph",
        "Where graph data is kept, and the limits on walks.",
    ),
    (
        "neo4j",
        "The Neo4j server, used only with graph.backend = \"neo4j\".",
    ),
    (
        "acquisition",
        "Filling gaps from the sites a tenant trusts, with an admin's approval.",
    ),
    ("cache", "Tenants kept in memory."),
    ("jobs", "The per-tenant write queue."),
];

pub const FIELDS: &[Field] = &[
    f(
        "server.host",
        Text,
        "The address the service listens on. 127.0.0.1 keeps it on this machine; 0.0.0.0 opens it to the network (use TLS in front of it then).",
    ),
    f("server.port", Integer, "The port it listens on."),
    f(
        "server.threads",
        Integer,
        "Threads the service computes on (searches, indexing, embedding). 0 means 4, or fewer on a machine with fewer CPUs.",
    ),
    f(
        "data.dir",
        Path,
        "The data folder: tenants, models, traces. Relative to where the service is started.",
    ),
    f(
        "identity.mode",
        Choice(&["api_key", "jwt"]),
        "How callers prove who they are: a tenant's API key with user and group headers, or a JWT signed by the host.",
    ),
    f(
        "identity.deployment",
        Choice(&["multi_tenant", "personal"]),
        "multi_tenant serves many tenants. personal serves one person: the default tenant is created on first start and a request without a user header acts as the default user.",
    ),
    f(
        "identity.default_tenant",
        Text,
        "Personal deployments: the tenant created on first start; its key is written to <data>/personal-api-key.txt.",
    ),
    f(
        "identity.default_user",
        Text,
        "Personal deployments: the user a request without the user header acts as. An ordinary user, without admin rights.",
    ),
    f(
        "identity.key_header",
        Text,
        "The header carrying the API key. For Authorization, a \"Bearer \" prefix is expected.",
    ),
    f(
        "identity.user_header",
        Text,
        "API key mode: the header naming the end user.",
    ),
    f(
        "identity.group_header",
        Text,
        "API key mode: the header listing the caller's groups, comma-separated.",
    ),
    f(
        "identity.audience_header",
        Text,
        "The header naming who an insert is for: tenant, user or group:<id>.",
    ),
    f(
        "identity.admin_key_env",
        Text,
        "The environment variable holding the service admin key, for scripts and host code calling the admin routes; unset or empty, only the console's signed-in admin reaches them.",
    ),
    f(
        "identity.public_key_file",
        Path,
        "JWT mode: the host's public key (PEM), relative to the config file.",
    ),
    f(
        "identity.algorithm",
        Choice(&[
            "RS256", "RS384", "RS512", "PS256", "PS384", "PS512", "ES256", "ES384", "EdDSA",
        ]),
        "JWT mode: the algorithm tokens are signed with.",
    ),
    f(
        "identity.tenant_claim",
        Text,
        "JWT mode: the claim holding the tenant id.",
    ),
    f(
        "identity.user_claim",
        Text,
        "JWT mode: the claim holding the user id.",
    ),
    f(
        "identity.groups_claim",
        Text,
        "JWT mode: the claim holding the group ids (a list).",
    ),
    f(
        "identity.issuer",
        OptionalText,
        "JWT mode: when set, a token's iss must be this.",
    ),
    f(
        "identity.audience",
        OptionalText,
        "JWT mode: when set, a token's aud must name this; when unset, a token with an aud is refused.",
    ),
    f(
        "store.file_name",
        Text,
        "The name of each tenant's SQLite file.",
    ),
    f(
        "librarian.model_file",
        Path,
        "The model file (provider, model, key), relative to the config file. Edit it on the Model page.",
    ),
    f(
        "librarian.max_tokens",
        Integer,
        "The most tokens one reply of the model may use.",
    ),
    f(
        "librarian.timeout_secs",
        Integer,
        "How long one model call may take, in seconds.",
    ),
    f(
        "librarian.related_pages",
        Integer,
        "How many related pages the librarian is shown when it files something (5 to 10 works best).",
    ),
    f(
        "librarian.read_window_tokens",
        Integer,
        "About how much of a document one reading step sees, in tokens; it grows when a concept runs past it.",
    ),
    f(
        "librarian.auto_approve",
        Boolean,
        "Approve every proposal at once instead of leaving it on the Review page: edits the verifier held (filing, lint's fixes, hand edits), sources acquisition found on the trusted sites, and agents' graph drafts. Each takes the same path as an admin's approval (code checks, the graph checked again, a source read and filed by the librarian), but nobody looks first: an edit the verifier rightly stopped is written, a screened outside page is filed, an agent's graph change is applied.",
    ),
    f(
        "converter.url",
        OptionalText,
        "Where the MarkItDown converter answers, such as http://127.0.0.1:8090. Unset, .doc, .xls and .ppt files are refused with what to do.",
    ),
    f(
        "converter.timeout_secs",
        Integer,
        "How long converting one file may take, in seconds, by the service or by MarkItDown.",
    ),
    f(
        "converter.pdf_reader",
        Path,
        "The pinakes-pdf binary, which reads PDFs (and draws a scan's pages for the model) in a process of its own with its memory capped. Unset, the one next to the service's executable.",
    ),
    f(
        "search.slots.memories",
        Integer,
        "Hits in the memories slot when a search doesn't say (at most 20).",
    ),
    f(
        "search.slots.knowledge",
        Integer,
        "Hits in the knowledge slot when a search doesn't say (at most 20).",
    ),
    f(
        "search.slots.relations",
        Integer,
        "Hits in the relations slot when a search doesn't say (at most 20).",
    ),
    f(
        "search.session_idle_minutes",
        Integer,
        "A search session (which pages an agent has read) ends after this many idle minutes.",
    ),
    f(
        "search.default_mode",
        Choice(&["keyword", "semantic", "auto"]),
        "The mode of a search that doesn't say. semantic needs embeddings on; auto ranks by keywords and meaning when they are on, by keywords when they are off.",
    ),
    f(
        "search.hop.top_memories",
        Integer,
        "How many of the top memories bring the knowledge pages they are anchored to into the knowledge slot.",
    ),
    advanced(
        "search.ablation.pin_names",
        Boolean,
        "Experiment switch: titles and aliases named in a query pin their pages to the top. Leave on.",
    ),
    advanced(
        "search.ablation.words",
        Boolean,
        "Experiment switch: search the words of each field (jieba, stemming). Leave on.",
    ),
    advanced(
        "search.ablation.bigrams",
        Boolean,
        "Experiment switch: search the character pairs of each field. Leave on.",
    ),
    f(
        "search.vector_graph_rows",
        Integer,
        "From this many page vectors on, a tenant keeps a graph index over them for meaning search (0: never).",
    ),
    f(
        "trace.enabled",
        Boolean,
        "Whether searches are traced to <tenant>/logs, for replays. Wiki versions are kept only while it is on.",
    ),
    f(
        "trace.retention_days",
        Integer,
        "Trace files older than this many days are removed; 0 keeps them.",
    ),
    Field {
        applies: Applies::Now,
        ..f(
            "embedding.enabled",
            Boolean,
            "Whether meaning-based search is on. Turning it on downloads the model (about 640 MB, once), keeps an 8-bit copy of it in its place (316 MB) and embeds every page once, without a restart; turning it off keeps the stored vectors.",
        )
    },
    f(
        "embedding.model",
        OptionalText,
        "The embedding model: jina-embeddings-v2-base-zh, the one the service runs. Unset, embeddings can't be turned on.",
    ),
    f(
        "graph.backend",
        Choice(&["sqlite", "neo4j"]),
        "Where graph data is kept: graph.sqlite in each tenant's folder, or a Neo4j server. A tenant's data is moved with pinakes-migrate-graph, with the service stopped.",
    ),
    f(
        "graph.max_depth",
        Integer,
        "The deepest graph walk a caller may ask for.",
    ),
    f(
        "graph.max_nodes",
        Integer,
        "The most nodes a graph walk returns.",
    ),
    f("neo4j.uri", Text, "The Neo4j server's address."),
    f("neo4j.user", Text, "The Neo4j user."),
    f(
        "neo4j.password_env",
        Text,
        "The environment variable holding the Neo4j password (never in this file).",
    ),
    f(
        "neo4j.database",
        OptionalText,
        "The Neo4j database; the server's default when unset.",
    ),
    f(
        "acquisition.enabled",
        Boolean,
        "Whether acquisition may run at all; each tenant turns it on too, with the sites it trusts.",
    ),
    f(
        "acquisition.daily_token_budget",
        Integer,
        "Tokens acquisition's own model calls may use per tenant and day.",
    ),
    f(
        "acquisition.crawl_pages_per_site",
        Integer,
        "The most pages read from one trusted site.",
    ),
    f(
        "acquisition.crawl_every_days",
        Integer,
        "Days before a tenant's sites are read again (at once when its list changes).",
    ),
    f(
        "acquisition.crawl_delay_ms",
        Integer,
        "The pause between two requests to a site, in milliseconds.",
    ),
    advanced(
        "acquisition.allow_private_hosts",
        Boolean,
        "Lets acquisition fetch from this machine and private networks. For tests only: never on in use.",
    ),
    f(
        "cache.idle_minutes",
        Integer,
        "A tenant unused this long gives up its in-memory copies; 0 keeps them.",
    ),
    f(
        "jobs.keep_finished_minutes",
        Integer,
        "Finished jobs stay visible this long.",
    ),
];

/// The descriptions in Chinese, by field path or section name.
pub const ZH: &[(&str, &str)] = &[
    ("server", "服务监听在哪里，以及一次能并行计算多少。"),
    ("data", "服务把数据放在哪里。"),
    (
        "identity",
        "调用方是谁：API 密钥或 JWT，以及携带用户、组和受众的请求头。",
    ),
    ("store", "每个租户的 SQLite 文件。"),
    (
        "librarian",
        "馆员，服务唯一调用的 LLM（只用于写入）。用哪个模型和密钥在模型文件里。",
    ),
    (
        "converter",
        "文件转换。Markdown、文本、HTML、Word、表格、PowerPoint 和 PDF 由服务自行读取；可选的 MarkItDown 转换器用于旧版二进制 Office 文件。",
    ),
    ("search", "检索如何排序，以及每个抽屉放多少条结果。"),
    ("trace", "检索轨迹，供回放使用。"),
    ("embedding", "语义检索：本地嵌入模型，默认关闭。"),
    ("graph", "图谱数据存在哪里，以及遍历的上限。"),
    (
        "neo4j",
        "Neo4j 服务器，仅在 graph.backend = \"neo4j\" 时使用。",
    ),
    ("acquisition", "从租户信任的网站补齐缺口，须经管理员批准。"),
    ("cache", "保留在内存中的租户。"),
    ("jobs", "每个租户的写入队列。"),
    (
        "server.host",
        "服务监听的地址。127.0.0.1 只对本机开放；0.0.0.0 对网络开放（此时请在前面加 TLS）。",
    ),
    ("server.port", "监听的端口。"),
    (
        "server.threads",
        "服务用于计算（检索、建索引、嵌入）的线程数。0 表示 4，CPU 较少时更少。",
    ),
    (
        "data.dir",
        "数据目录：租户、模型、轨迹。相对于服务启动的位置。",
    ),
    (
        "identity.mode",
        "调用方如何证明身份：租户 API 密钥加用户和组请求头，或由宿主签发的 JWT。",
    ),
    (
        "identity.deployment",
        "multi_tenant 服务多个租户。personal 只服务一个人：首次启动时创建默认租户，不带用户头的请求视为默认用户。",
    ),
    (
        "identity.default_tenant",
        "个人版：首次启动时创建的租户；其密钥写入 <data>/personal-api-key.txt。",
    ),
    (
        "identity.default_user",
        "个人版：不带用户头的请求所代表的用户。普通用户，没有管理员权限。",
    ),
    (
        "identity.key_header",
        "携带 API 密钥的请求头。为 Authorization 时需带 \"Bearer \" 前缀。",
    ),
    (
        "identity.user_header",
        "API 密钥模式：指明终端用户的请求头。",
    ),
    (
        "identity.group_header",
        "API 密钥模式：列出调用方所属组（逗号分隔）的请求头。",
    ),
    (
        "identity.audience_header",
        "指明录入内容给谁的请求头：tenant、user 或 group:<id>。",
    ),
    (
        "identity.admin_key_env",
        "存放服务管理员密钥的环境变量，供脚本和业务后端调用管理接口；未设置或为空时，只有登录控制台的管理员能用这些接口。",
    ),
    (
        "identity.public_key_file",
        "JWT 模式：宿主的公钥（PEM），相对于配置文件。",
    ),
    ("identity.algorithm", "JWT 模式：令牌的签名算法。"),
    ("identity.tenant_claim", "JWT 模式：存放租户 id 的声明。"),
    ("identity.user_claim", "JWT 模式：存放用户 id 的声明。"),
    (
        "identity.groups_claim",
        "JWT 模式：存放组 id（列表）的声明。",
    ),
    (
        "identity.issuer",
        "JWT 模式：设置后，令牌的 iss 必须等于它。",
    ),
    (
        "identity.audience",
        "JWT 模式：设置后，令牌的 aud 必须包含它；未设置时，带 aud 的令牌会被拒绝。",
    ),
    ("store.file_name", "每个租户 SQLite 文件的名称。"),
    (
        "librarian.model_file",
        "模型文件（提供方、模型、密钥），相对于配置文件。在“模型”分页编辑。",
    ),
    ("librarian.max_tokens", "模型单次回复最多使用的令牌数。"),
    ("librarian.timeout_secs", "单次模型调用的超时，单位秒。"),
    (
        "librarian.related_pages",
        "馆员整理内容时参考的相关页面数（5 到 10 效果最好）。",
    ),
    (
        "librarian.read_window_tokens",
        "阅读文档时每一步大约看到的令牌数；一个概念超出时会扩大。",
    ),
    (
        "librarian.auto_approve",
        "所有待审内容立即自动批准，不再留在“审阅”页等待：校验员拦下的编辑（归档、体检修复、人工修改）、采集从信任网站找到的外部来源、智能体提交的图谱草稿。每一项都走和管理员批准相同的流程（代码检查、按当前图谱重新检查、由馆员阅读并归档外部来源），但没有人先看：校验员正确拦下的编辑会写入，筛查过的外部网页会归档，智能体的图谱修改会生效。",
    ),
    (
        "converter.url",
        "MarkItDown 转换器的地址，例如 http://127.0.0.1:8090。未设置时，.doc、.xls 和 .ppt 会被拒绝并说明怎么办。",
    ),
    (
        "converter.timeout_secs",
        "转换一个文件的超时（服务自身或 MarkItDown），单位秒。",
    ),
    (
        "converter.pdf_reader",
        "pinakes-pdf 程序，在单独的进程里读取 PDF（并把扫描件的页面画成图片交给模型）并限制其内存。未设置时用服务程序同目录下的那个。",
    ),
    (
        "search.slots.memories",
        "检索未指定时，记忆抽屉的结果数（最多 20）。",
    ),
    (
        "search.slots.knowledge",
        "检索未指定时，知识抽屉的结果数（最多 20）。",
    ),
    (
        "search.slots.relations",
        "检索未指定时，关系抽屉的结果数（最多 20）。",
    ),
    (
        "search.session_idle_minutes",
        "检索会话（记录智能体读过哪些页面）空闲这么多分钟后结束。",
    ),
    (
        "search.default_mode",
        "检索未指定时的模式。semantic 需要开启向量嵌入；auto 在开启向量时同时按关键词和语义检索，关闭时只用关键词。",
    ),
    (
        "search.hop.top_memories",
        "排名靠前的多少条记忆会把它们锚定的知识页面带入知识抽屉。",
    ),
    (
        "search.ablation.pin_names",
        "实验开关：查询中出现的标题和别名会把对应页面置顶。请保持开启。",
    ),
    (
        "search.ablation.words",
        "实验开关：检索各字段的分词结果（jieba、词干）。请保持开启。",
    ),
    (
        "search.ablation.bigrams",
        "实验开关：检索各字段的字对。请保持开启。",
    ),
    (
        "search.vector_graph_rows",
        "页面向量达到这么多行后，租户为语义检索建图索引（0：从不）。",
    ),
    (
        "trace.enabled",
        "是否把检索轨迹记录到 <tenant>/logs，供回放使用。只有开启时才保留知识库旧版本。",
    ),
    (
        "trace.retention_days",
        "超过这么多天的轨迹文件会被删除；0 表示永久保留。",
    ),
    (
        "embedding.enabled",
        "是否开启语义检索。开启时会下载模型（约 640 MB，仅一次）、只保留由它生成的 8 位版本（316 MB），并把每个页面嵌入一次，无需重启；关闭后保留已存的向量。",
    ),
    (
        "embedding.model",
        "嵌入模型：jina-embeddings-v2-base-zh，服务唯一支持的模型。未设置时无法开启向量嵌入。",
    ),
    (
        "graph.backend",
        "图谱数据存在哪里：各租户目录中的 graph.sqlite，或 Neo4j 服务器。租户数据用 pinakes-migrate-graph 迁移（需停服）。",
    ),
    ("graph.max_depth", "调用方可请求的最大遍历深度。"),
    ("graph.max_nodes", "一次遍历最多返回的节点数。"),
    ("neo4j.uri", "Neo4j 服务器地址。"),
    ("neo4j.user", "Neo4j 用户。"),
    (
        "neo4j.password_env",
        "存放 Neo4j 密码的环境变量（密码从不写入此文件）。",
    ),
    ("neo4j.database", "Neo4j 数据库；未设置时用服务器默认库。"),
    (
        "acquisition.enabled",
        "是否允许运行采集；每个租户还要自己开启并列出信任的网站。",
    ),
    (
        "acquisition.daily_token_budget",
        "采集自身的模型调用每个租户每天可用的令牌数。",
    ),
    (
        "acquisition.crawl_pages_per_site",
        "每个信任网站最多读取的页面数。",
    ),
    (
        "acquisition.crawl_every_days",
        "隔多少天重新读取租户的网站（列表变化时立即读取）。",
    ),
    (
        "acquisition.crawl_delay_ms",
        "对同一网站两次请求之间的间隔，单位毫秒。",
    ),
    (
        "acquisition.allow_private_hosts",
        "允许采集访问本机和内网。仅供测试：使用中切勿开启。",
    ),
    (
        "cache.idle_minutes",
        "租户闲置这么久后释放其内存副本；0 表示一直保留。",
    ),
    ("jobs.keep_finished_minutes", "已完成的任务保留可见的时长。"),
];

/// A field's or section's description in Chinese.
pub fn zh(key: &str) -> &'static str {
    ZH.iter()
        .find(|(k, _)| *k == key)
        .map(|(_, v)| *v)
        .unwrap_or("")
}

/// One field as the console shows it.
#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct FieldView {
    pub path: &'static str,
    pub section: &'static str,
    pub description: &'static str,
    pub description_zh: &'static str,
    #[serde(flatten)]
    pub kind: FieldKind,
    pub applies: Applies,
    pub advanced: bool,
    pub default: Value,
    /// Whether the console may change it (see [`editable`]).
    pub editable: bool,
    /// The value the running service uses.
    pub value: Value,
    /// The value in the file, when it differs from the running one (a restart applies it).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub saved: Option<Value>,
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct SectionView {
    pub name: &'static str,
    pub description: &'static str,
    pub description_zh: &'static str,
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct SettingsView {
    /// The config file, when the service was started from one; without one nothing can be
    /// written.
    pub file: Option<String>,
    pub sections: Vec<SectionView>,
    pub fields: Vec<FieldView>,
    /// Whether a saved change waits for a restart.
    pub restart_needed: bool,
}

/// Every leaf of a config as `path → value`.
pub fn leaves(config: &Config) -> BTreeMap<String, Value> {
    fn walk(prefix: &str, v: &Value, out: &mut BTreeMap<String, Value>) {
        match v {
            Value::Object(m) => {
                for (k, x) in m {
                    let path = if prefix.is_empty() {
                        k.clone()
                    } else {
                        format!("{prefix}.{k}")
                    };
                    walk(&path, x, out);
                }
            }
            other => {
                out.insert(prefix.to_owned(), other.clone());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(
        "",
        &serde_json::to_value(config).unwrap_or_default(),
        &mut out,
    );
    out
}

/// The settings of the running service, beside what its file says now. `embedding_on` is the
/// embedding switch as it is now, when there is one: it applies at once, so the service runs
/// with it rather than with the value it started with.
pub fn view(running: &Config, embedding_on: Option<bool>) -> SettingsView {
    let mut now = leaves(running);
    if let Some(on) = embedding_on {
        now.insert("embedding.enabled".into(), Value::Bool(on));
    }
    let defaults = leaves(&Config::default());
    let saved = running.file.as_deref().and_then(|p| {
        let text = std::fs::read_to_string(p).ok()?;
        Config::from_toml_str(&text).ok().map(|c| leaves(&c))
    });
    let fields: Vec<FieldView> = FIELDS
        .iter()
        .map(|field| {
            let value = now.get(field.path).cloned().unwrap_or(Value::Null);
            let in_file = saved.as_ref().and_then(|s| s.get(field.path).cloned());
            FieldView {
                path: field.path,
                section: field.path.split('.').next().unwrap_or_default(),
                description: field.description,
                description_zh: zh(field.path),
                kind: field.kind,
                applies: field.applies,
                advanced: field.advanced,
                default: defaults.get(field.path).cloned().unwrap_or(Value::Null),
                editable: editable(field.path),
                saved: in_file.filter(|v| *v != value),
                value,
            }
        })
        .collect();
    SettingsView {
        file: running.file.as_ref().map(|p| p.display().to_string()),
        sections: SECTIONS
            .iter()
            .map(|(name, description)| SectionView {
                name,
                description,
                description_zh: zh(name),
            })
            .collect(),
        restart_needed: fields
            .iter()
            .any(|f| f.saved.is_some() && f.applies == Applies::Restart),
        fields,
    }
}

/// Whether the console may change a field. One naming a secret's variable isn't changed from
/// the console: pointed at another variable (and an address elsewhere), it would send that
/// variable's value out.
pub fn editable(path: &str) -> bool {
    !path.ends_with("_env")
}

/// Checks a change against its field: known, editable, and of the field's kind. `null` puts the
/// field back to its default (its line is removed).
pub fn check(path: &str, value: &Value) -> Result<Option<toml_edit::Value>, String> {
    let field = FIELDS
        .iter()
        .find(|f| f.path == path)
        .ok_or_else(|| format!("{path} is not a setting"))?;
    if !editable(path) {
        return Err(format!(
            "{path} names the variable a secret is read from: change it in the config file by hand"
        ));
    }
    if value.is_null() {
        return Ok(None);
    }
    let bad = || format!("{path} takes {}", describe(field.kind));
    let v: toml_edit::Value = match field.kind {
        Text | Path | OptionalText => value.as_str().ok_or_else(bad)?.into(),
        Integer => value.as_i64().filter(|n| *n >= 0).ok_or_else(bad)?.into(),
        Boolean => value.as_bool().ok_or_else(bad)?.into(),
        Choice(options) => {
            let s = value
                .as_str()
                .filter(|s| options.contains(s))
                .ok_or_else(bad)?;
            s.into()
        }
    };
    Ok(Some(v))
}

fn describe(kind: FieldKind) -> String {
    match kind {
        Text | Path => "text".into(),
        OptionalText => "text, or null to unset it".into(),
        Integer => "a whole number, 0 or more".into(),
        Boolean => "true or false".into(),
        Choice(options) => format!("one of {}", options.join(", ")),
    }
}

/// `docs/configuration.md`, generated from [`SECTIONS`] and [`FIELDS`].
pub fn reference() -> String {
    let defaults = leaves(&Config::default());
    let mut out = String::from(
        "# Configuration reference\n\nGenerated from `src/settings.rs` (`PINAKES_WRITE_REFERENCE=1 cargo test --lib settings`); do not edit by hand. Each field is set in `config.toml` (or the file `PINAKES_CONFIG` names) and shown, explained and edited on the console's Settings page. Secrets are never in this file: they are environment variables (the admin key, the Neo4j password) or in the model file.\n",
    );
    for (section, description) in SECTIONS {
        out.push_str(&format!(
            "\n## [{section}]\n\n{description}\n\n| Field | Kind | Default | Applies | What it does |\n| --- | --- | --- | --- | --- |\n"
        ));
        for field in FIELDS
            .iter()
            .filter(|f| f.path.split('.').next() == Some(section))
        {
            let key = &field.path[section.len() + 1..];
            let default = match defaults.get(field.path) {
                Some(Value::Null) | None => "unset".to_owned(),
                Some(v) => format!("`{v}`"),
            };
            let applies = match field.applies {
                Applies::Now => "at once",
                Applies::Restart => "after a restart",
            };
            let mut what = field.description.replace('|', "\\|");
            if field.advanced {
                what.push_str(" (advanced)");
            }
            out.push_str(&format!(
                "| `{key}` | {} | {default} | {applies} | {what} |\n",
                describe(field.kind).replace('|', "\\|")
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn every_field_of_the_config_is_explained_and_nothing_else() {
        let leaves = leaves(&Config::default());
        for path in leaves.keys() {
            let field = FIELDS.iter().find(|f| f.path == path);
            assert!(
                field.is_some_and(|f| !f.description.trim().is_empty()),
                "{path} has no description in settings::FIELDS"
            );
        }
        for f in FIELDS {
            assert!(
                leaves.contains_key(f.path),
                "{} is not a config field",
                f.path
            );
            let section = f.path.split('.').next().unwrap();
            assert!(
                SECTIONS.iter().any(|(s, _)| *s == section),
                "{} is in no section",
                f.path
            );
        }
        assert_eq!(FIELDS.len(), leaves.len());
        for key in FIELDS
            .iter()
            .map(|f| f.path)
            .chain(SECTIONS.iter().map(|(s, _)| *s))
        {
            assert!(
                !zh(key).is_empty(),
                "{key} has no Chinese description in settings::ZH"
            );
        }
        for (key, _) in ZH {
            assert!(
                FIELDS.iter().any(|f| f.path == *key) || SECTIONS.iter().any(|(s, _)| s == key),
                "settings::ZH names {key}, which is no field or section"
            );
        }
    }

    #[test]
    fn the_reference_document_is_up_to_date() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/configuration.md");
        let now = reference();
        if std::env::var_os("PINAKES_WRITE_REFERENCE").is_some() {
            std::fs::write(&path, &now).unwrap();
        }
        let written = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            written.replace("\r\n", "\n") == now,
            "docs/configuration.md is out of date: PINAKES_WRITE_REFERENCE=1 cargo test --lib settings"
        );
    }

    #[test]
    fn changes_are_checked_against_their_field() {
        assert!(check("server.port", &serde_json::json!(8081)).is_ok());
        assert!(check("server.port", &serde_json::json!("8081")).is_err());
        assert!(check("search.default_mode", &serde_json::json!("fuzzy")).is_err());
        assert!(check("nope.key", &serde_json::json!(1)).is_err());
        assert!(check("converter.url", &Value::Null).unwrap().is_none());
        for field in ["neo4j.password_env", "identity.admin_key_env"] {
            assert!(check(field, &serde_json::json!("DEEPSEEK_API_KEY")).is_err());
        }
    }

    #[test]
    fn the_console_is_told_which_fields_it_may_change() {
        let v = view(&Config::default(), None);
        let editable = |path: &str| v.fields.iter().find(|f| f.path == path).unwrap().editable;
        assert!(!editable("neo4j.password_env"));
        assert!(!editable("identity.admin_key_env"));
        assert!(editable("server.port"));
        // Every field the console shows as editable passes the check with its own value.
        for f in v.fields.iter().filter(|f| f.editable) {
            assert!(check(f.path, &f.value).is_ok(), "{}", f.path);
        }
    }
}
