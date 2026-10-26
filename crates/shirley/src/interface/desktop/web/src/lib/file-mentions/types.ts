// Domain types for reference chips. Migrated from shiwen's entity-mentions, but narrowing
// "article/Space/document/group/conversation/material" down to objects that actually exist
// in Shirley: **files and directories** in the workspace, and **skills** selected via the
// `/` entry.
//
// Every reference kind shares the same chip infrastructure (inline nodes in the editor +
// message echo via `InlineReferences`); they differ only in `kind`: the icon, and the
// meaning handed to the Rust side after serialization (file/dir -> path; skill -> skill
// name; image -> pasted data URL; media -> media-library artifact id).
//
// `media` is the **uploaded** kind: a file the user dropped / picked was already put into
// the media library by the harness, so the turn references an artifact id instead of
// carrying bytes. That is what makes a 500MB video attachable without base64 in the log.

export type ReferenceKind = "file" | "dir" | "skill" | "image" | "media";

export type FileReference = {
  /** file/dir = workspace path; skill = skill name. */
  path: string;
  /** Display name (the text shown on the chip). */
  name: string;
  kind: ReferenceKind;
  /** Skill description (used for the tooltip / popover subtitle only when `kind === "skill"`). */
  description?: string;
  /**
   * Pasted image payload (`kind === "image"`): a `data:` URL (base64 inline). The bytes live
   * only in the editor / this round's request — they are never persisted into the session log
   * (the log keeps the text-only shape, same as `@` file references).
   */
  dataUrl?: string;
  /**
   * Media-library artifact id (`kind === "media"`). This is the payload handed to the Rust
   * side: `runtime::user_content` looks the entry up in the library, so the model only sees
   * a short reference line plus a renderable part.
   */
  artifactId?: string;
  /** MIME type of a `media` reference, for the chip icon / echo card. */
  mime?: string;
  /** Size in bytes of a `media` reference (display only). */
  bytes?: number;
};

/** Whether this is a non-file skill reference (sent with `/` skill-activation semantics). */
export function isSkillReference(reference: FileReference): boolean {
  return reference.kind === "skill";
}
