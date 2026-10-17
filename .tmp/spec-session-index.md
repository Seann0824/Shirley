# Task S: FileSessionCatalog 懒加载 index.json（只改 src/session.rs）

## 目标
把 `FileSessionCatalog::list()` 从「每次全量扫目录 + 每个 .jsonl 全文件读一遍数 turns/preview」改为「读一份 `index.json` 缓存 + 按 mtime 对账、只重算变化项」。这是云端 session 懒加载的首要性能瓶颈（`list()` 被频繁调用，O(所有 session × 全文件)）。

## 严格边界（重要）
- **只允许修改 `src/session.rs` 一个文件**。不得改任何其它文件（Rust / 前端 / docs 都不要动）。
- **不得修改 `SessionCatalog` trait 的方法签名**、不得修改 `SessionEntry` 的字段、不得修改 `SessionStore` trait。
- 现有公开行为必须保持：`list()` 仍返回按 `modified_ms` 降序的 `Vec<SessionEntry>`；`label` 仍是「有 title 用 title，否则用 name」；`preview`/`turns` 语义不变。
- 现有全部 `src/session.rs` 单元测试必须继续通过；如需要可新增测试，但不得删改已有断言语义。

## index.json 规格
- 路径：`<catalog.dir>/index.json`（即 `dir` 目录下，与 `*.jsonl` / `*.title` 同目录）。
- 内容 schema（用 serde 定义内部结构，可 `#[derive(Serialize, Deserialize)]`，加 `#[serde(default)]` 容忍缺失字段）：
  ```json
  {"version":1,"sessions":[{"name":"...","label":"...","preview":"...","modified_ms":123,"turns":2}]}
  ```
  `version` 固定 1；读取时若 version 不匹配则当作空缓存全量重算。
- `index.json` 本身不是 `.jsonl`，read_dir 循环里因扩展名过滤天然会跳过它，确认无副作用。

## list() 新实现（对账算法）
1. 尝试读 `index.json` → `HashMap<name, CachedEntry>`；读不到/解析失败 → 空 map（**不报错**，退化为全量重算）。
2. `read_dir(self.dir)`：`NotFound` 视为空（沿用现有行为）。遍历每个 `.jsonl`：
   - 取 `name`（file_stem）、`modified_ms`（file metadata modified → unix ms，沿用现有代码逻辑）。
   - **title 每次都重读**：`label = self.read_title(name).unwrap_or(name)`（.title 是极小的单文件，开销可忽略；这样 `rename`（只改 .title、不改 .jsonl mtime）能立即生效，无需 hook rename）。
   - 若缓存里有同名条目且 `cached.modified_ms == 当前 modified_ms` → 复用 `cached.preview` / `cached.turns`（label 用上面重读的）。
   - 否则（新文件 / mtime 变化）→ 调 `summary_of(&path)` 得到 `(preview, turns)` 重算。
   - 组装 `SessionEntry`，同时记录进「新缓存 map」。
3. 磁盘上已消失的文件：自然不在新 map 里（对账即删除，无需显式处理）。
4. 比较「新缓存」与「旧缓存」是否有差异（name 集合、每个 name 的 modified_ms/preview/turns/label 任一不同）。**有差异才回写** `index.json`：
   - 原子写：先写 `<dir>/index.json.tmp`（或带进程/线程唯一后缀的临时名，避免并发互相覆盖），`create_dir_all(dir)`，再 `std::fs::rename(tmp, index.json)`（复用 `src/project.rs` 里 temp+rename 的惯用法）。
   - **回写是 best-effort**：失败不得让 `list()` 报错（缓存坏了下次重算即可），忽略错误或仅 stderr 一行。
5. 返回按 `modified_ms` 降序排序的 entries（沿用现有排序）。

## 并发
- `list()` 可能被多线程调用（`&self`）。原子 temp+rename 已能避免读到半个文件；回写竞争可接受（best-effort 缓存）。**不需要**为 index 加锁、不需要改 struct 字段（保持 `FileSessionCatalog { dir: PathBuf }` 单字段，`create_lazy` 里 `FileSessionCatalog { dir: self.dir.clone() }` 的构造不用改）。

## 验收
- `cargo build` 通过（无需 feature）。
- `cargo test session::` 通过（现有测试 + 你新增的对账测试）。
- 建议新增 2 个测试：① 首次 list 生成 index.json 且内容正确；② 第二次 list 命中缓存（preview/turns 复用），改 .jsonl mtime 后重算、删文件后条目消失、rename 后 label 更新。

## 交付报告
回复：改了哪些函数、index.json 结构与回写时机、新增测试名与 `cargo test session::` 结果（贴关键输出）。
