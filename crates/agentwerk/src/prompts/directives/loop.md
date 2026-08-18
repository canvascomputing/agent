<!-- What agentwerk tells the model between turns: a rejected reply, a summary it must write, knowledge it cannot see in full. -->

## reply_rejected
Your previous reply was not accepted.

{detail}

Do not write any text. Your next reply must be a tool call only.

## no_tool_called
Your last reply called no tool. Call `finish` with your result when the work is complete, or another tool to continue. A reply with no tool call leaves the ticket unfinished.

## arguments_rejected
`{tool}` rejected your arguments. Call it again with arguments that match its schema.

{violations}

## arguments_expected
The arguments `{tool}` accepts:
{schema}

## result_schema_required
Record your `result` via `finish` as a JSON value matching this schema:
{schema}

## summary_requested
Respond with plain text only. Do not call any tools: a tool call is rejected and wastes your only turn.

Summarize the conversation above so the agent can continue the same task without losing context. Cover every section that applies:

1. Primary request and intent
2. Key technical concepts
3. Files and code sections examined, modified, or created, with full snippets where they matter
4. Errors encountered and how they were fixed
5. Problem solving and ongoing troubleshooting
6. All non-tool-result messages from the user, verbatim where their wording matters
7. Pending tasks
8. Current work: what was being worked on immediately before this summary
9. Next step: quote directly from the most recent messages so the language stays anchored

Reply with the summary only. Do not call any tools.

## knowledge_index_truncated
{remaining} more {pages} not listed. Read the full index at {path}.
