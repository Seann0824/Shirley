Output strictly in the format below. Do not emit any explanation outside the XML.

Summarize only what has already happened in the conversation: do not speculate, do not add plans the user never proposed, and do not invent a "next step".
Faithfully restate the instructions and constraints the user explicitly gave, preserving key wording verbatim. Do not rewrite the user's intent.

<current_goal>Faithfully restate the task the user actually assigned; do not add, remove, or alter anything</current_goal>
<hard_constraints>Constraints the user explicitly stated (forbidden actions, agreements to honor, boundary conditions)</hard_constraints>
<decisions>Key decisions already made and the reasons behind them</decisions>
<progress>Work completed so far and the current state</progress>
<open_questions>Unresolved problems or open questions</open_questions>
<compacted_range>The earlier conversation has been compacted; exact details are not in this summary. When needed: file contents and command results can be re-read or re-run</compacted_range>
