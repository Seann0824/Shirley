// `@` 引用的领域类型。迁移自 shiwen 的 entity-mentions，但把
// 「文章/Space/文稿/分组/对话/资料」收敛为 Shirley 里真实存在的对象：
// 工作区里的**文件与目录**。剥离 shiwen 的 material / OCR / 删除资料等定制逻辑。

export type FileReference = {
  path: string;
  name: string;
  kind: "file" | "dir";
};
