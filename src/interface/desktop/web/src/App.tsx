import { useCallback, useEffect, useRef, useState } from "react";
import {
  AiConversationTranscript,
  type AiTranscriptMessage,
} from "@/components/ai/AiConversationTranscript";
import { AiChatComposer } from "@/components/ai/AiChatComposer";
import { AssistantMessage } from "@/components/AssistantMessage";
import { ModelSelector } from "@/components/ai/ModelSelector";
import { agentBridge, type StreamHandle } from "@/lib/bridge";
import type { AiToolExecution } from "@/types/ai";

let messageSeq = 0;
const nextId = () => `m${++messageSeq}`;

export function App() {
  const [messages, setMessages] = useState<AiTranscriptMessage[]>([]);
  // 工具执行按 assistant 消息 id 分组；放 state 才能触发重渲染
  // （直接改 ref 数组不会重渲染——曾踩过）。
  const [toolRuns, setToolRuns] = useState<Record<string, AiToolExecution[]>>({});
  const [input, setInput] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [model, setModel] = useState("");
  const handleRef = useRef<StreamHandle | null>(null);

  useEffect(() => {
    void agentBridge()
      .then((bridge) => bridge.modelName().then(setModel).catch(() => {}));
  }, []);

  const send = useCallback(async () => {
    const text = input.trim();
    if (!text || busy) return;
    setInput("");
    setError("");
    setBusy(true);

    const assistantId = nextId();
    setMessages((prev) => [
      ...prev,
      { id: nextId(), role: "user", content: text, status: "complete" },
      { id: assistantId, role: "assistant", content: "", status: "streaming" },
    ]);
    setToolRuns((prev) => ({ ...prev, [assistantId]: [] }));

    const patchMessage = (patch: (m: AiTranscriptMessage) => AiTranscriptMessage) => {
      setMessages((prev) => prev.map((m) => (m.id === assistantId ? patch(m) : m)));
    };
    const patchTools = (patch: (tools: AiToolExecution[]) => AiToolExecution[]) => {
      setToolRuns((prev) => ({ ...prev, [assistantId]: patch(prev[assistantId] ?? []) }));
    };

    const bridge = await agentBridge();
    handleRef.current = await bridge.send(text, (event) => {
      switch (event.type) {
        case "content_delta":
          patchMessage((m) => ({ ...m, content: m.content + event.text }));
          break;
        case "reasoning_delta":
          // 第一期思考流不单独成区，先并入正文之后的处理留待 M3。
          break;
        case "tool_started":
          patchTools((tools) => [
            ...tools,
            {
              id: event.call_id,
              call_id: event.call_id,
              tool_name: event.name,
              status: "running",
              summary: "",
              input: safeParseArgs(event.arguments),
            },
          ]);
          break;
        case "tool_finished":
          patchTools((tools) =>
            tools.map((tool) =>
              tool.call_id === event.call_id
                ? {
                    ...tool,
                    status: event.ok ? "complete" : "error",
                    summary: event.output.slice(0, 200),
                    error: event.ok ? null : event.output,
                  }
                : tool,
            ),
          );
          break;
        case "error":
          setError(event.message);
          patchMessage((m) => ({ ...m, status: "error" }));
          setBusy(false);
          handleRef.current = null;
          break;
        case "finished":
          patchMessage((m) => ({ ...m, status: "complete" }));
          setBusy(false);
          handleRef.current = null;
          break;
        default:
          break;
      }
    });
  }, [busy, input]);

  const stop = useCallback(() => {
    handleRef.current?.cancel();
    handleRef.current = null;
    setBusy(false);
  }, []);

  const switchModel = useCallback(async (value: string) => {
    try {
      const bridge = await agentBridge();
      await bridge.setModel(value);
      setModel(value);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, []);

  return (
    <div className="flex h-screen w-full min-h-0 flex-col bg-canvas">
      <header className="flex h-12 shrink-0 items-center border-b border-line px-4">
        <span className="font-display text-body-sm text-ink">Shirley</span>
      </header>
      <AiConversationTranscript
        messages={messages}
        conversationKey="main"
        extraContentKey={busy ? "busy" : ""}
        emptyContent={
          <p className="px-1 py-10 text-center text-body-sm text-muted">
            开始和 Shirley 对话吧。
          </p>
        }
        renderMessageContent={(message) =>
          message.role === "assistant" ? (
            <AssistantMessage
              content={message.content}
              streaming={message.status === "streaming"}
              executions={toolRuns[String(message.id)] ?? []}
            />
          ) : (
            message.content
          )
        }
      />
      {error && (
        <p className="mx-3 mb-1 text-body-sm text-danger-ink" role="alert">
          {error}
        </p>
      )}
      <div className="shrink-0 px-3 pb-3">
        <AiChatComposer
          id="shirley-composer"
          label="发送消息"
          value={input}
          placeholder="给 Shirley 发消息…"
          busy={busy}
          onValueChange={setInput}
          onSend={() => void send()}
          onStop={stop}
          trailingAction={
            <ModelSelector current={model} onSelect={(value) => void switchModel(value)} />
          }
        />
      </div>
    </div>
  );
}

function safeParseArgs(args: string): Record<string, unknown> | undefined {
  try {
    const parsed = JSON.parse(args);
    return typeof parsed === "object" && parsed ? parsed : undefined;
  } catch {
    return undefined;
  }
}
