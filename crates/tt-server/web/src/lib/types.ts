export interface Verdict {
  current_stream: CurrentStream | null;
  top_todo: TopTodo | null;
  aligned: boolean | null;
  wip: WipStatus;
  alignment_share: number | null;
  pending_proposals: number;
  machines: MachineFreshness[];
  classifier: ClassifierHealth;
}

export interface CurrentStream {
  stream_id: string;
  name: string;
  since: string;
}

export interface TopTodo {
  id: string;
  text: string;
  stream_slug: string | null;
}

export interface WipStatus {
  in_flight: StreamActivity[];
  limit: number;
  wind_down_candidate: string | null;
}

export interface StreamActivity {
  stream_id: string;
  name: string;
  direct_ms: number;
  delegated_ms: number;
}

export interface MachineFreshness {
  label: string;
  last_sync_at: string | null;
}

export interface ClassifierHealth {
  last_success_at: string | null;
  last_failure_at: string | null;
  last_error: string | null;
  consecutive_failures: number;
}

export interface TimelineData {
  window: { start: string; end: string };
  streams_active: TimelineStream[];
  idle_gaps: IdleGap[];
  db_version: number;
}

export interface IdleGap {
  start: string;
  end: string;
  duration_minutes: number;
}

export interface TimelineStream {
  stream: Stream;
  focus_intervals: Interval[];
  delegated_intervals: Interval[];
  events: TimelineEvent[];
}

export interface Stream {
  id: string;
  name: string | null;
  slug: string | null;
  description: string | null;
  color: string | null;
  created_at: string;
  updated_at: string;
  time_direct_ms: number;
  time_delegated_ms: number;
  first_event_at: string | null;
  last_event_at: string | null;
  needs_recompute: boolean;
}

export interface Interval {
  start: string;
  end: string;
}

export interface TimelineEvent {
  timestamp: string;
  kind: 'user_message' | 'subagent_start' | 'session_start' | 'session_end';
  session_id: string | null;
  /** null for point kinds that aren't session markers (user_message, subagent_start). */
  todo_linked: boolean | null;
}

export interface Todo {
  id: string;
  text: string;
  section: string;
  priorities: { slug: string; value: number }[];
  stream_slug: string | null;
  due: string | null;
  when: string | null;
  linked_agent_count: number;
}

export interface Session {
  harness: string;
  session_id: string;
  stream: { name: string; slug: string } | null;
  machine_label: string | null;
  start_time: string;
  duration_ms: number;
  last_activity: string;
  linked_todo_text: string | null;
}

export interface Proposal {
  id: string;
  created_at: string;
  target: ProposalTarget;
  confidence: number;
  reasoning: string;
  scope: ProposalScope;
}

export type ProposalTarget =
  | { kind: 'existing'; name: string; slug: string; stream_id: string }
  | { kind: 'new'; name: string; description: string };

export type ProposalScope =
  | { kind: 'session'; count: number }
  | { kind: 'events'; count: number };
