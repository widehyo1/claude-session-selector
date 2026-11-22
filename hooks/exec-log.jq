{
  v: 1,
  session_id,
  agent_id,
  tool_use_id,
  ended_at_ms: (now * 1000 | floor),
  duration_ms,
  outcome: (
    if .hook_event_name == "PostToolUseFailure" then (if .is_interrupt then "interrupted" else "error" end)
    elif (.tool_response | type) == "object" and .tool_response.backgroundTaskId then "backgrounded"
    elif (.tool_response | type) == "object" and .tool_response.interrupted then "interrupted"
    else "ok" end),
  background_task_id: ((.tool_response | objects | .backgroundTaskId) // null)
}
