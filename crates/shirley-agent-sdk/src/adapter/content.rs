//! Shared image / content-part encoding helpers.
//!
//! The three protocols differ in the *shape* of an image block but agree on the
//! semantic ("here is a text part" / "here is an image reference"). The shape
//! differences are contained here so the per-protocol encoders stay thin.
//!
//! **Text-only fast path**: when a content carries no image, each protocol keeps
//! its historical text-only encoding byte-for-byte (ChatCompletions: a plain
//! string; Responses: `[{type:input_text}]`; Anthropic: `[{type:text}]`). This
//! preserves the KV prefix cache for the overwhelmingly common text case.

use serde_json::{json, Value};

use super::AdapterError;
use crate::message::{Content, ContentPart, ImageDetail, ImagePart, ImageSource};

fn detail_str(detail: ImageDetail) -> &'static str {
    match detail {
        ImageDetail::Low => "low",
        ImageDetail::High => "high",
        ImageDetail::Auto => "auto",
    }
}

/// ChatCompletions: `string` when text-only, otherwise an array of
/// `{type:text}` / `{type:image_url, image_url:{url, detail}}` parts.
pub(crate) fn chat_content(content: &Content) -> Result<Value, AdapterError> {
    if content.is_text_only() {
        return Ok(Value::String(content.as_text()));
    }
    let mut parts = Vec::new();
    for part in content.as_parts() {
        match part {
            ContentPart::Text { text } => parts.push(json!({ "type": "text", "text": text })),
            ContentPart::Image(image) => match &image.source {
                ImageSource::Url { url } => {
                    let mut image_url = json!({ "url": url });
                    if let Some(detail) = image.detail {
                        image_url["detail"] = json!(detail_str(detail));
                    }
                    parts.push(json!({ "type": "image_url", "image_url": image_url }));
                }
                ImageSource::FileId { file_id } => {
                    return Err(AdapterError::Encode(format!(
                        "ChatCompletions image source does not accept a file_id ({file_id}); \
                         provide a url (including a data: URL)"
                    )));
                }
            },
        }
    }
    Ok(Value::Array(parts))
}

/// Responses: `[{type:input_text}]` when text-only (historical shape), otherwise
/// `{type:input_image, image_url|file_id, detail}` parts alongside the text ones.
pub(crate) fn responses_content(content: &Content) -> Result<Value, AdapterError> {
    if content.is_text_only() {
        return Ok(json!([{ "type": "input_text", "text": content.as_text() }]));
    }
    let mut parts = Vec::new();
    for part in content.as_parts() {
        match part {
            ContentPart::Text { text } => {
                parts.push(json!({ "type": "input_text", "text": text }))
            }
            ContentPart::Image(image) => {
                let mut block = json!({ "type": "input_image" });
                match &image.source {
                    ImageSource::Url { url } => block["image_url"] = json!(url),
                    ImageSource::FileId { file_id } => block["file_id"] = json!(file_id),
                }
                if let Some(detail) = image.detail {
                    block["detail"] = json!(detail_str(detail));
                }
                parts.push(block);
            }
        }
    }
    Ok(Value::Array(parts))
}

/// Anthropic: `[{type:text}]` when text-only (historical shape), otherwise
/// `{type:image, source:{type:base64|url, ...}}` parts. A `data:` URL is split
/// into the base64 source shape Anthropic expects; a plain URL becomes a url source.
pub(crate) fn anthropic_content(content: &Content) -> Result<Value, AdapterError> {
    if content.is_text_only() {
        return Ok(json!([{ "type": "text", "text": content.as_text() }]));
    }
    let mut blocks = Vec::new();
    for part in content.as_parts() {
        match part {
            ContentPart::Text { text } => blocks.push(json!({ "type": "text", "text": text })),
            ContentPart::Image(image) => blocks.push(anthropic_image(image)?),
        }
    }
    Ok(Value::Array(blocks))
}

fn anthropic_image(image: &ImagePart) -> Result<Value, AdapterError> {
    match &image.source {
        ImageSource::Url { url } => {
            if let Some(rest) = url.strip_prefix("data:") {
                let (meta, data) = rest.split_once(',').ok_or_else(|| {
                    AdapterError::Encode("malformed data: image URL (missing comma)".to_owned())
                })?;
                let media_type = meta.strip_suffix(";base64").unwrap_or(meta);
                Ok(json!({
                    "type": "image",
                    "source": { "type": "base64", "media_type": media_type, "data": data },
                }))
            } else {
                Ok(json!({
                    "type": "image",
                    "source": { "type": "url", "url": url },
                }))
            }
        }
        ImageSource::FileId { file_id } => Err(AdapterError::Encode(format!(
            "Anthropic image source does not accept a file_id ({file_id}); \
             provide a url or a data: URL"
        ))),
    }
}

/// Whether any message carries an image part. Used by the send-time capability guard.
///
/// Scans `User` **and** `Tool` (a computer-use screenshot is a tool result); a tool
/// image that the capability guard missed would be sent to a model that cannot read it.
pub(crate) fn messages_have_images(messages: &[crate::message::Message]) -> bool {
    messages.iter().any(|message| match message {
        crate::message::Message::User { content } => content.has_images(),
        crate::message::Message::Tool {
            content: Some(content),
            ..
        } => content.has_images(),
        _ => false,
    })
}

/// Remove every image part from the messages, keeping text intact.
///
/// Used by the silent image-degradation path (`adapter::invoke`): when the model cannot
/// read images the request is resent with the images dropped instead of failing. A message
/// left with no text (an image-only turn) gets a short placeholder so the request stays
/// valid. Messages without images are cloned unchanged, so an all-text request is
/// byte-for-byte identical (KV prefix cache).
pub(crate) fn strip_images(messages: &[crate::message::Message]) -> Vec<crate::message::Message> {
    use crate::message::Message;
    messages
        .iter()
        .map(|message| match message {
            Message::User { content } => Message::User {
                content: strip_content_images(content),
            },
            Message::Tool {
                tool_call_id,
                content,
            } => Message::Tool {
                tool_call_id: tool_call_id.clone(),
                content: content.as_ref().map(strip_content_images),
            },
            other => other.clone(),
        })
        .collect()
}

fn strip_content_images(content: &Content) -> Content {
    let kept: Vec<ContentPart> = content
        .as_parts()
        .iter()
        .filter(|part| matches!(part, ContentPart::Text { .. }))
        .cloned()
        .collect();
    if kept.is_empty() {
        Content::text("[image omitted: this model cannot read images]")
    } else {
        Content::parts(kept)
    }
}

/// The image parts of `content` as a fresh content, or `None` when there are none.
///
/// Protocols whose tool-result slot is text-only (ChatCompletions / Responses) split a
/// tool result into a text part and a trailing user message; this extracts the images
/// for that trailing message.
pub(crate) fn images_only(content: &Content) -> Option<Content> {
    let images: Vec<ContentPart> = content
        .as_parts()
        .iter()
        .filter(|part| matches!(part, ContentPart::Image(_)))
        .cloned()
        .collect();
    (!images.is_empty()).then(|| Content::parts(images))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{ContentPart, ImagePart};

    fn mixed() -> Content {
        Content::parts(vec![
            ContentPart::Text {
                text: "look".to_owned(),
            },
            ContentPart::Image(ImagePart::url("https://x/y.png").with_detail(ImageDetail::High)),
        ])
    }

    #[test]
    fn text_only_stays_plain_string_on_chat() {
        let value = chat_content(&Content::text("hi")).unwrap();
        assert_eq!(value, Value::String("hi".to_owned()));
    }

    #[test]
    fn chat_mixed_uses_image_url_block() {
        let value = chat_content(&mixed()).unwrap();
        let parts = value.as_array().unwrap();
        assert_eq!(parts[1]["type"], "image_url");
        assert_eq!(parts[1]["image_url"]["url"], "https://x/y.png");
        assert_eq!(parts[1]["image_url"]["detail"], "high");
    }

    #[test]
    fn responses_file_id_maps_to_input_image() {
        let content = Content::parts(vec![ContentPart::Image(ImagePart::file_id("file-1"))]);
        let value = responses_content(&content).unwrap();
        assert_eq!(value[0]["type"], "input_image");
        assert_eq!(value[0]["file_id"], "file-1");
    }

    #[test]
    fn guard_detects_images_only_in_user_messages() {
        use crate::message::Message;
        let text_only = vec![Message::User {
            content: "hi".into(),
        }];
        assert!(!messages_have_images(&text_only));

        let with_image = vec![Message::User {
            content: Content::parts(vec![
                ContentPart::Text {
                    text: "look".to_owned(),
                },
                ContentPart::Image(ImagePart::url("https://x/y.png")),
            ]),
        }];
        assert!(messages_have_images(&with_image));
    }

    #[test]
    fn anthropic_data_url_becomes_base64_source() {
        let content = Content::parts(vec![ContentPart::Image(ImagePart::url(
            "data:image/png;base64,AAAA",
        ))]);
        let value = anthropic_content(&content).unwrap();
        assert_eq!(value[0]["source"]["type"], "base64");
        assert_eq!(value[0]["source"]["media_type"], "image/png");
        assert_eq!(value[0]["source"]["data"], "AAAA");
    }
}
