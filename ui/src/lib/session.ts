import type { DeliveryResult, Meeting, Segment, SessionEvent, Speaker } from "./api";
import { insertSorted } from "./format";

export type Phase = "idle" | "starting" | "recording" | "finishing" | "done";

export interface SessionState {
  phase: Phase;
  transcriptId: string | null;
  title: string;
  engine: string;
  startedAt: number | null;
  stoppedAt: number | null;
  segments: Segment[];
  partials: Partial<Record<Speaker, string>>;
  levels: Record<Speaker, number>;
  deliveries: DeliveryResult[];
  /** Calendar meeting that triggered this recording, if any. */
  meeting: Meeting | null;
}

export const initialSession: SessionState = {
  phase: "idle",
  transcriptId: null,
  title: "",
  engine: "",
  startedAt: null,
  stoppedAt: null,
  segments: [],
  partials: {},
  levels: { me: 0, them: 0 },
  deliveries: [],
  meeting: null,
};

export type SessionAction =
  | { type: "reset" }
  | { type: "starting"; title: string }
  | { type: "start-failed" }
  | { type: "recording"; transcriptId: string; title: string; startedAt: number; meeting?: Meeting | null }
  | { type: "stopping" }
  | { type: "event"; event: SessionEvent };

export function sessionReducer(state: SessionState, action: SessionAction): SessionState {
  switch (action.type) {
    case "reset":
      return initialSession;
    case "starting":
      return { ...initialSession, phase: "starting", title: action.title };
    case "start-failed":
      return { ...state, phase: "idle" };
    case "recording":
      return {
        ...state,
        phase: "recording",
        transcriptId: action.transcriptId,
        title: action.title,
        startedAt: action.startedAt,
        meeting: action.meeting ?? state.meeting,
      };
    case "stopping":
      return { ...state, phase: "finishing", stoppedAt: Date.now(), levels: { me: 0, them: 0 } };
    case "event":
      return applyEvent(state, action.event);
  }
}

function applyEvent(state: SessionState, event: SessionEvent): SessionState {
  switch (event.type) {
    case "started":
      return { ...state, engine: event.engine };
    case "partial":
      return { ...state, partials: { ...state.partials, [event.speaker]: event.text } };
    case "segment": {
      const { [event.segment.speaker]: _, ...partials } = state.partials;
      return { ...state, segments: insertSorted(state.segments, event.segment), partials };
    }
    case "levels":
      return state.phase === "recording" ? { ...state, levels: { me: event.me, them: event.them } } : state;
    case "finished":
      return {
        ...state,
        phase: "finishing",
        stoppedAt: state.stoppedAt ?? Date.now(),
        partials: {},
        levels: { me: 0, them: 0 },
      };
    case "delivery":
      return { ...state, deliveries: [...state.deliveries, event.result] };
    case "done":
      return { ...state, phase: "done" };
    case "error":
      return state;
  }
}
