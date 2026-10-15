# What capabilities should AgentSDK contain?

As an SDK, it should normally contain the most basic Agent building blocks and not involve any
business-specific customization. For the consumer, it should be possible to simply assemble their
own business-side Agent capabilities through AgentSDK.

So the question comes back to: what is a basic capability? Which basic capabilities should it
contain?

Looking from the bottom up:

1. The starting point of an Agent should be letting the user send "Hello Agent" to any vendor's AI
   model through the Agent and get the corresponding reply back. In this process, the actual caller
   should not need to care about each vendor's call differences or protocol compatibility.
2. The former only shows that a user message can really be delivered to the AI model side and the
   model's reply can be obtained. From a real scenario, a task often contains complex steps, e.g.
   user intent, AI thinking, AI action, AI observation, intent completion, and so on -- a whole
   ReAct loop that drives a complex task to completion. As everyone knows, calling a vendor's model
   directly through an API is stateless. That is, if the user introduced their name in the previous
   round, the model will not know "who am I?" when the second call asks it. Because it does not
   include the previous message history. So, as an SDK, we need to provide the business side with
   the ability to carry historical messages. Essentially, we need to send the history to the AI
   model, and in reverse return the AI model's output to the business side in the SDK's standardized
   format, so the business side no longer needs to care about the underlying data transport and
   parsing. The business side only needs to perceive: I can pass history in from here, I can get the
   model's messages out of this hook, etc.
3. A complex task necessarily involves perceiving, thinking about, and acting on the outside world.
   How does the model get external information? The model calls Tools provided by the business side
   to obtain the relevant business data, and finally, through reasoning and tool calls, keeps pushing
   the problem forward until the business-side problem is solved. For the business side, different
   problems may need different tools, so the SDK's ability to extend tools is necessarily very
   important. So we need to support tool management. When the AI wants to call a tool, that
   necessarily involves a tool call, and a tool call necessarily needs a corresponding argument
   parsing specification, including tool name, incoming arguments, return value, error handling,
   etc. -- a standardized format for tools, and this format must necessarily be provided by the SDK.

From the above, we can simply plan a directory structure:

```sh
shirley_agent_sdk
    dto
        UserMessage
        SystemMessage
        AssistantMessage
        ReasonMessage
        ToolCall
        ToolResult
        OpenaiCompatible <-> DTO
        AnthropicCompatible <-> DTO
    tool
        ToolTrait
            StreamToolTrait
            NormalToolTrait
        ToolManager
            ToolMangerTrait

    mod.rs
```

Let's do the adapter layer first; once it's done we can build the Agent flow on top of the adapter
layer.

We have now made the Message enum and the Tool Manager used to manage tools and calls. So for a
complete ReAct Agent SDK, we need to smooth over the entire ReAct AI-calling flow for the user, so
that when using it they only need to register tools and actually send a task? Then for message
management and the format of the AI's return value, the user does not need to care how to handle it.
The user only needs to know that writing the SDK this way will give them the data. So our SDK should
expose an Agent capability outward, where the user declares the Agent's model config and some tools
they support, etc.

Internally the SDK does an architecture of message management and tool management etc., uniformly
stitched together at the Runtime layer, and what is exposed outward should not be some of the SDK's
internal methods and details; what is exposed outward should only be some of the necessary types and
macros for tool definitions, plus some format for custom message conversion? Because the SDK is too
basic, we should be able to uniformly get messages, etc., or modify the current message, through the
Agent.

I am wondering: what exactly do we need to maintain for a message in essence? The raw JSON Value
structure, or the Message type. If it's the raw Value structure, sending it to the AI should not
need a conversion layer, but when the user gets a Message we need to provide a programmable data
type structure. How should this logic be weighed inside the SDK?

```rs
let mut agent = Agent::builder()
    .model(model_config)
    .system_prompt("You are Shirley, the person you love most is Lelouch, and you can do anything to protect Lelouch. The Black Knights led by Lelouch caused a landslide that buried your father under the city. Because of this, you hate Zero deeply; later you discover that Zero is actually Lelouch. Torn between hatred and love, you finally decide to protect Lelouch, but no longer love him.")
    .tool(send_meesage::tool())
    .tool(read_book::tool())
    .build()?;
agent.run("What is your name").await;
agent.run_stream("Help me read this book", |agent_event| {
    println!("{agent_event}");
}).await;
agent.messages();
agent.clear_messages();
agent.replace_messages(saved_messages);
```

I am wondering what data the run and run_stream methods here should return. Maybe the Messages newly
produced during this call? Or something else. In essence, one run is one turn, and one turn has one
or more user messages plus multiple AI messages and tool-call messages. What is the use of returning
this out? Let me think -- at most for display. Yes, the business side should take this message and
consume it for display, or take this message to evaluate the path of this task and see what can be
optimized. So we want to fully return the messages and tool-call path of this task.

An agent_event is essentially a consumption event exposed outward by the Agent Event SDK. Anything
external that wants to implement a UI or something can get it based on this event; every time a
corresponding event is produced internally, this function is called to expose the event to the
business-side code for consumption.

Here we have implemented the concrete send-message and tool-calling capabilities. But it seems we
have not handled reason-related content, and we have not verified the actual cache hit rate, so we
cannot ensure the prefix is the same. So we need to verify whether cache hits are in a normal range.
Actually, just use some simple cases and run some API interface tests to see how many cache hits
there are. Set a threshold.

For the difference between run_stream and run, who assembles the complete AI return content? Wait, let
me think whether the AI interface will return the complete content after the stream ends, and we just
need to catch it.

Currently I have a design problem: how the #tool tool handles parameters. Currently I convert all
parameters into Value by default, following the OpenAI ChatCompletions format. This means that once I
need to be compatible with the Responses or Anthropic message format, I cannot perceive the format of
the parameter list and convert it to the corresponding protocol's format. I understand that the #tool
macro should, when converting the function, treat parameters as a standardized template, or wrap an
extra type in between -- just the common JSON types -- and then we write conversion rules on top of
this middle layer.

The problem I want to solve first is that deserializing JSON currently always goes through
hand-written gets. I will define a Rust json adapter layer, so that value -> Rust Type is direct.

I have now built the usage layer; the metrics I currently care about are input, output, cache, etc.;
others can be added later if needed.

1. I found I was writing in the wrong direction. At first I planned to write an AgentSDK, and after I
   finished the whole ReAct chain I did not know what other capabilities the SDK should have. So I
   woke up: the SDK is essentially a collection of tools, so that the next time you use it you do not
   need to pay attention to the details again, like Vue -- you do not need to care how Vue updates
   the DOM, only about what capability you want to implement.

- From the perspective of Vue's appearance: at first, because you had to manually update the DOM when
  your business data changed. Once this operation happened a lot, you would find it was repetitive
  logic. So this can become a framework capability, fixing the logic that does not change for you.
- Back to AgentSDK, which capabilities should be fixed? I do not know yet, but how do I get an SDK?
  I roughly know now: I must first focus on one Agent application scenario and implement it, and in
  the process of implementing it keep making decisions to discover which capabilities will be useful
  the next time an Agent is implemented -- then those capabilities can go under AgentSDK.

2. So I decided to design a common CodingAgent, and in this focused scenario try to use the Agent I
   designed to migrate an original TS backend project to a Rust backend. This process will
   necessarily run into many problems:

- Such as task planning
- Long task execution
- Behavior restriction, permission control, etc.

However, I still know two problems that need solving:

- Regarding reason-related content, the framework currently does not save reason-related data, and
  we currently return the stream at the granularity of each round of messages, not yet in the form of
  TextDelta or ReasonDelta. At the same time, we also need to handle the actual API adapter side
  supporting stream, which means we encode and decode the data.
- Another problem is that the tool's parameter list is not implemented as our common protocol, but
  directly converted to the OpenAI format. This requires us to design an adapter layer, so that later
  we can convert to other message formats through this adapter layer.
- Now the UI works, but I feel I really want to add a memory capability to this Agent, so I do not
  have to ask it questions every time. But the problem is: will the memory capability cause
  pollution? What should go into memory? I am thinking about this. At the same time we also need to
  avoid occupying too much context; only recall context when it is really needed. I think I can refer
  to a paper I saw today that says it scores higher than the memo benchmark.
- I can refer to the tool-design paper for how to describe a tool clearly to the AI:
  https://arxiv.org/abs/2609.15397?utm_source=chatgpt.com
- I can refer to the memory-design paper:
  https://arxiv.org/abs/2609.11060?utm_source=chatgpt.com
  - Paper title: "Grounding Agent Memory: Environment-Probing Curation for Enterprise Agents".
    Core: an async curator that can only see "completed trajectories" will retain errors /
    over-generalization / outdated knowledge, because one trajectory is only a single, local,
    possibly-wrong observation of the environment. The solution is to give the curator a set of
    **read-only environment tools** after the task ends, and to verify candidate memories via
    propose-probe-commit (without changing the task agent / memory representation / write
    permission; without a safe read-only surface, fall back to pure-trajectory curation).
  - So a finished task cannot be directly summarized and stored; we must first "probe the
    environment and verify" -- the trajectory is not the best logic; only after verification does it
    settle, adding a verification layer in front of the memory store. (The paper also has a
    non-writing distiller doing pre-compression, with the boundary that it cannot see terminal
    feedback and treats the rollout as partial evidence rather than ground truth.)

At present, invoke has unified the streaming and non-streaming return interface; non-streaming calls
also return a Stream handle, but only return AdapterEvent::Finished, not ContentDelta and
ReasoningDelta.

- Later we need to parse the real stream SSE return value according to this protocol
  (https://api-docs.deepseek.com/api/create-chat-completion/)
- We need to throw the parsed reason and content outward in real time at the adapter layer when
  stream is true, and at the same time, when the finish_reason appears at the end of this AI
  conversation round, throw the complete message returned by the AI this round outward via Finished.

SSE standard spec: https://html.spec.whatwg.org/multipage/server-sent-events.html
How do we judge that an SSE event is complete?

- \n means a newline; keep sending the data of this event
- \n\n (CRLF/CR/LF) means the next line is a blank line, i.e. the current event has ended; by
  convention we can then take out the complete JSON data format received this time. Because an event
  provided by an SSE provider is a JSON, we can just parse this json directly.

```text
data: hello

```

This one is done sending.

Then, when do we judge that this round of sending has ended? By convention, when our event parsing
sees finish_reason appear and data: [DONE] appears, or a network problem (a network problem may
involve reconnection -- we do not need to consider network problems for now).
The parsing of the SSE stream, and the TUI's consumption and rendering, are already done.

A pain point I hit when using codex is that the project itself was generated by codex, and when I
wanted to run it I did not know how to run the project. This forced me to either have the AI run it
and then note down the command, or read for myself how to run the project.

- I think a more reasonable way is: the AI directly generates a row of interactive buttons based on
  the current project. When we click one, it runs the corresponding script itself. This way we do not
  need to care what the command is; we only need to say what we want. The advantage is that our
  scenario is very clear, and we do not need to ask the AI and wait for its reply.
- Actually the main pain point is that we have to feed a prompt to let the AI run the project, but if
  the AI can offer such a prompt option after the task completes, and clicking the option is the
  command, then this interaction is fairly intuitive.

Pasting long text is currently broken.

Next I plan to add workshop, conversions, and sqlite message-management logic, and at the same time
support tasks' custom tasks for convenient self-triggered tasks. But here, I deleted the Read tool;
I feel only one bash is enough, and the only thing to do is permission control over different
commands.

(Backfill) In practice, keeping only bash has an obvious problem: `cat` on a large file has no output
cap, so the model can flood the context with one raw read. So add the read capability back in the
form of `read_file` -- not restoring the old `read`, but a **controllable version**: the path is
constrained by the workspace, with a double cap on lines (default 200 / max 2000) and bytes (64KB),
and suspected secrets and binary files rejected by default. The core idea: a capability should be
controllable, rather than simply deleting or opening it up.

- These concepts should not belong to AgentSDK's capabilities; they are essentially capabilities the
  business side should encapsulate, so I understand it is fine to design them on the Coding Agent
  side. But for permission control, I understand the SDK must have a permission-control hook it can
  throw outward, so that the outside can interact with the user to approve tool execution, etc.

It suddenly occurs to me that any tool permission restriction seems useless. If, in a scenario
without finely-grained features, only bash is provided, it can handle many functions. But when it
comes to permission control, it is very hard to do permission control for bash. Because bash is
really too flexible; you cannot restrict the permission of AI-dynamically-generated Python code
through matching logic. I feel it is better to just throw it into a sandbox without too many
restrictions and let the AI complete the task inside the sandbox itself.

The principle for adding tools:

- If adding a tool makes the Agent behave better, then it is worth adding a tool.

I found a problem about context compaction: after compaction the AI forgot the task the user assigned
before compaction. Then I looked and saw that the compaction summary even gave a next step. But that
step was not something I proposed before compaction. It seems we need to preserve the user
instructions from before compaction, and part of the AI conversation? How many turns exactly should
be preserved? This needs some thought. Also we need to optimize the compaction instruction so that
compaction only extracts key information and does not overstep.

- The purpose of context compaction should be to deal with questions that may arise in the future?
  It is not simply compressing the context into a summary and being done; if a future query asks for
  information not present in the summary, the model will not know. Because the Compact Generation
  Game is essentially a decision handed to the model, we cannot define what to preserve for
  different tasks; what different tasks need to preserve may all differ. This uncertainty forces us
  to hand this to the LLM rather than using a fixed programmatic compaction strategy. One thing we
  need to do is: in subsequent queries, if context loss is caused by compaction, the model should be
  able to recall the previous content through a specific tool or memory recall. This is a very
  useful optimization we can do to handle context compaction. Normally it is about letting the LLM
  look up the context information of the previous conversation, which involves Memory and RAG
  knowledge.
- This paper https://arxiv.org/html/2608.01326v1 is about compaction algorithm guidance and
  evaluation schemes.
  - The value of compaction is not determined by "what the original information looks like" alone,
    but by "what will be asked in the future".

```text
[System Instructions]

[Current Task State]
How to update and maintain this content needs thought

[Important Constraints]
Some key constraints

[Relevant Memory / Summary]
Here is the LLM's summary of the non-retained region's content.

[Recent Conversation]
How much of this content to retain also needs thought

[Current User Message]
```

Research shows that using XML tags improves the LLM's attention to this content. I saw in a THink
article that even without enabling thinking, merely using tags like <thinking> guides the LLM to
think. Leveraging this principle, we can convert the context we have built into a label format.

```text
<current_goal>
Implement context compaction.
</current_goal>

<hard_constraints>
- Do not modify message.rs
- Preserve API compatibility
</hard_constraints>

<decisions>
...
</decisions>
```

Just normalized errors, so internally use the concrete tool's error, and by implementing
std::io::Error it is easy to convert to AgentError via ? and expose it outward.

Besides bash, we also need to provide an apply_patch, so we can directly observe what the Agent
actually changed this round, and send it to the view layer for consumption. And we can run a rollback
of this round's changes, rather than only through git diff.

## How to write the system prompt

> Prompt design principle: if the model can do things with very few prompts, then a lot of
> constraints need to be added.

1. Role

- Work boundary
- Execution environment

2. Objective

- Clarify the highest goal, and the priority

Context compaction needs a retry mechanism; sometimes a compaction failure succeeds on retry.

- The compaction cut point is computed too late, so after the model over-thinks it has already
  exceeded the context window, and at that point there is no context space left to support
  compaction.
- Consider how to optimize? Retry as a fallback for this case, or maybe we should do tiered
  compaction. Prioritize compacting the tool output first...

Another problem: every time a new session is opened for a project, the AI has to repeatedly explore
the project, which wastes a lot of time and tokens.
Persistent memory is really important. Every time I constrain it not to write test files together
with source files, the AI does the same thing every time and I have to repeat it.

- Here we can provide Agent.md to the AI, but then there is another problem. What should Agent.md
  contain?
- It should contain things that do not change often, such as project architecture, directory
  responsibilities, etc.? If they change, we should have the AI update them promptly.
- For a new project, the first step is often to generate an agent.md file.

Ugh, the AI did not get an AI workspace and actually searched from the root directory. This
information still needs to be manually given to the AI when building the system prompt, to narrow the
search scope.

Ugh, the AI model crashed halfway through the chat, so I did not dare close the window; there is no
session persistence and model switching.

I am wondering whether to do semantic search, top-k recall, using a local model, multi-query
generation, or generating a fake answer and then doing similarity matching.
However, if this Coding Agent is to be used by other users, either we provide a coding-agent model
service ourselves, or users can configure their own model capability. But honestly, if every
user-facing configuration requires editing env, that is a bit programmer-ish, yet it is inevitably
cumbersome.

Currently I understand persistence should be done inside the SDK logic for the conversation, but I am
wondering,
that is, our SDK interface should be abstract, and after the outside implements some trait,
but we cannot restrict the choice of persistence; our SDK only needs to expose the interface
declaration needed for internal persistence,
and then we can call the persistence function it implements? Whether it is sqlite, postgresql, mysql,
or even jsonl, I understand our SDK does not need to care about the concrete persistence technical
details.

Some files are too large; the moment the AI reads one it triggers context compaction.

I found that after compact, the AI always re-explores, then compacts again and re-explores again. The
core problem is that it did not record a todo? What was done before is forgotten after compaction.

## Memory

I now want to do a memory capability, so I do not have to explain some of my tasks every time. And
the workspace space does not need to be explored again.
But what I want is personal memory, so it becomes an assistant that understands you. A coding agent
is essentially just the first step; the core is a personal agent. But I also do not understand how
to do eval-related logic, and I feel it is not that important -- it is not too late to do it once we
really hit a wall.

I think we can start with a simple text for memory, without going so complex with vectors,
similarity, conflict detection right away.
The memory load timing is when the user opens a new session: automatically send the memory to the AI,
or the user sends it in the first message.

The memory storage format: I understand markdown is enough.

```md
memory/
├── episodic/
│ └── 2026/
│ └── 10/
│ └── 2026-10-10-memory-design.md
├── people/
│ └── alice.md
├── projects/
│ └── coding-agent.md
├── preferences/
│ └── communication.md
└── procedures/
└── weekly-review.md
```

```md
---
type: preference
subject: work_mode
created_at: 2026-10-10T17:30:00+08:00
valid_from: 2026-10-10
supersedes: mem_2025_remote_work
---

User currently prefers working in person rather than fully remote.
```

What is worth becoming a memory?

- User preference: e.g. the Agent makes a mistake, and after the user corrects it, should this
  constraint become a preference? No. If the user says "from now on do it this way", then it can
  100% become a user preference; if they merely point out an error to be fixed, that is only a
  correction of the current behavior and does not mean doing it this way from now on -- you can ask
  the user and then decide.
- The user's basic information: as a personal agent it should proactively help the user record
  basic information. This gives the user a true personal assistant, rather than having to explain
  who I am and what my hobbies are every time.
- Proactive memory does not mean remembering everything.

How are memories corrected?

- When memories conflict, how to correct them? Do we need to modify historical memories? I think
  not. From the perspective of human memory, we do not forget things that happened before. It is
  just that as time changes, we create some new memories. It does not mean the previous memories
  disappear, so each of our memory storage formats needs a timeline.

Emmm, I feel I think too much and always want to choose a very good solution. But a good solution is
completed in practice; without practice, who knows whether it is good. For now I will just do a
simple one -- document-stored memory -- and really solve problems when I hit them.

I feel markdown looks hard to retrieve. I feel it is not as good as a vector index, with the raw text
as evidence.
