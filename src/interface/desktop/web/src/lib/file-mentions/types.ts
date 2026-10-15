// Domain types for reference chips. Migrated from shiwen's entity-mentions, but narrowing
// "article/Space/document/group/conversation/material" down to objects that actually exist
// in Shirley: **files and directories** in the workspace, and **skills** selected via the
// `/` entry.
//
// Both reference kinds share the same chip infrastructure (inline nodes in the editor +
// message echo via `InlineReferences`); they differ only in `kind`: the icon, and the
// meaning handed to the Rust side after serialization (file/dir -> path; skill -> skill name).

export type ReferenceKind = "file" | "dir" | "skill";

export type FileReference = {
  /** file/dir = workspace path; skill = skill name. */
  path: string;
  /** Display name (the text shown on the chip). */
  name: string;
  kind: ReferenceKind;
  /** Skill description (used for the tooltip / popover subtitle only when `kind === "skill"`). */
  description?: string;
};

/** Whether this is a non-file skill reference (sent with `/` skill-activation semantics). */
export function isSkillReference(reference: FileReference): boolean {
  return reference.kind === "skill";
}
