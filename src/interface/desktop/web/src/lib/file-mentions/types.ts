// 引用 chip 的领域类型。迁移自 shiwen 的 entity-mentions，但把
// 「文章/Space/文稿/分组/对话/资料」收敛为 Shirley 里真实存在的对象：
// 工作区里的**文件与目录**，以及 `/` 入口选中的**技能**。
//
// 两类引用共用同一套 chip 基建（编辑区内联节点 + 消息回显 `InlineReferences`），
// 差别只在 `kind`：图标、以及序列化后交给 Rust 侧的含义（file/dir → 路径；skill → 技能名）。

export type ReferenceKind = "file" | "dir" | "skill";

export type FileReference = {
  /** file/dir = 工作区路径；skill = 技能名。 */
  path: string;
  /** 展示名（chip 上显示的文本）。 */
  name: string;
  kind: ReferenceKind;
  /** 技能描述（仅 `kind === "skill"` 时用于 tooltip / 浮层副标题）。 */
  description?: string;
};

/** 是否是非文件的技能引用（发送时要走 `/` 技能激活语义）。 */
export function isSkillReference(reference: FileReference): boolean {
  return reference.kind === "skill";
}
