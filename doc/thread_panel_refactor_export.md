# Thread Panel — files for refactor

Snapshot of every web file involved in rendering thread replies and reconciling
optimistic / live-stream / persisted-history state. Use this as the input to a
clean-room rewrite of the rendering logic.

Files included (paths relative to `web/`):

- `src/pages/ChatView.tsx` — orchestrator: state, pending-humans dedup, hooks wiring
- `src/components/organisms/ThreadPanel.tsx` — panel + reply cards + thinking placeholder + scroll
- `src/lib/chatBody.ts` — `decodeBody`, `foldHistoryIntoBubbles`
- `src/hooks/useThreadStream.ts` — SSE subscription + invalidate-on-chunk
- `src/hooks/useThreads.ts` — G1 list + G2 history (with `pollMs` fallback)
- `src/hooks/useSubmitPrompt.ts` — POST /prompts mutation
- `src/stores/threadStream.ts` — Zustand store of live bubbles
- `src/types/api.ts` — wire types
- `src/lib/api.ts` — fetch helpers

---

## `src/pages/ChatView.tsx`

```tsx
import { useEffect, useMemo, useState } from "react";
import { ChatLayout } from "../components/templates/ChatLayout";
import { ChannelHeader } from "../components/organisms/ChannelHeader";
import { Composer } from "../components/organisms/Composer";
import { MessageList } from "../components/organisms/MessageList";
import { Sidebar } from "../components/organisms/Sidebar";
import {
  ThreadPanel,
  type PendingHumanReply,
} from "../components/organisms/ThreadPanel";
import { MenuRail } from "../components/organisms/MenuRail";
import { useAgents } from "../hooks/useAgents";
import { useThreadHistory, useThreads } from "../hooks/useThreads";
import { useThreadStream } from "../hooks/useThreadStream";
import { useSubmitPrompt } from "../hooks/useSubmitPrompt";
import {
  DEMO_AGENTS,
  DEMO_HISTORY,
  DEMO_HUMAN_POSTER,
  DEMO_REPLIES,
  DEMO_THREADS,
  DEMO_USER,
} from "../lib/demo";
import { decodeBody } from "../lib/chatBody";

const CHANNEL = "general";

/** Force demo fixtures via `?demo=1` (or empty backend). */
function isDemoMode(): boolean {
  if (typeof window === "undefined") return false;
  return new URLSearchParams(window.location.search).get("demo") === "1";
}

export function ChatView() {
  const forcedDemo = isDemoMode();
  const [selectedRoot, setSelectedRoot] = useState<string | null>(
    forcedDemo ? DEMO_THREADS[0]!.root_request_id : null,
  );
  // When set, the channel feed is filtered to threads where this agent is
  // the human's first recipient (`first_agent.id === selectedAgentId`) —
  // a hop where it received from another agent does not match.
  const [selectedAgentId, setSelectedAgentId] = useState<string | null>(null);
  const [showPanel, setShowPanel] = useState(true);
  // Optimistic human follow-ups, keyed by the thread root they belong to.
  // Cleared once the matching `session_messages` row appears in G2 history.
  const [pendingHumans, setPendingHumans] = useState<
    Map<string, PendingHumanReply[]>
  >(new Map());

  const agentsQ = useAgents();
  const threadsQ = useThreads();
  // Poll the active thread's history while we're waiting on a submission.
  // SSE handles the happy path, but the dev proxy can drop chunks; the
  // poll guarantees the agent's reply appears within ~2s even when no
  // stream chunks arrive. Stops as soon as `pendingHumans` empties.
  const hasPendingForActive = !!(
    selectedRoot && (pendingHumans.get(selectedRoot)?.length ?? 0) > 0
  );
  const historyQ = useThreadHistory(
    forcedDemo ? null : selectedRoot,
    hasPendingForActive ? 2_000 : false,
  );
  useThreadStream(forcedDemo ? null : selectedRoot);
  const submit = useSubmitPrompt();

  // Demo fallback — `?demo=1` forces fixtures; otherwise backend data is
  // authoritative and demo only fills in when the backend returns nothing.
  const isDemo = forcedDemo || (!agentsQ.isLoading && !threadsQ.data?.length);
  const agents = isDemo ? DEMO_AGENTS : (agentsQ.data ?? []);
  const threads = isDemo ? DEMO_THREADS : (threadsQ.data ?? []);

  const defaultAgent = useMemo(
    () => agents.find((a) => a.is_default) ?? agents[0],
    [agents],
  );

  const visibleThreads = useMemo(
    () =>
      selectedAgentId
        ? threads.filter((t) => t.first_agent.id === selectedAgentId)
        : threads,
    [threads, selectedAgentId],
  );

  const selectedAgent = useMemo(
    () =>
      selectedAgentId
        ? (agents.find((a) => a.id === selectedAgentId) ?? null)
        : null,
    [agents, selectedAgentId],
  );

  const selectedThread = useMemo(
    () => threads.find((t) => t.root_request_id === selectedRoot) ?? null,
    [threads, selectedRoot],
  );

  const history = isDemo
    ? selectedRoot === DEMO_THREADS[0]!.root_request_id
      ? DEMO_HISTORY
      : []
    : (historyQ.data ?? []);

  const replies = isDemo ? DEMO_REPLIES : history;

  const onSubmit = async (input: { content: string; agent_id?: string }) => {
    if (isDemo) return;
    const agent_id = selectedAgentId ?? input.agent_id ?? defaultAgent?.id;
    if (!agent_id) return;
    const res = await submit.mutateAsync({
      content: input.content,
      agent_id,
    });
    setSelectedRoot(res.request_id);
  };

  const onThreadReply = async (input: { content: string }) => {
    if (isDemo || !selectedThread) return;
    const root = selectedThread.root_request_id;
    // Auto-prefix the receiver's @handle so the optimistic bubble matches
    // what the fold renders for the persisted row (`prefixWithReceiver`).
    const agentName = selectedThread.first_agent.name;
    const text = input.content.startsWith(`@${agentName}`)
      ? input.content
      : `@${agentName} ${input.content}`;
    const optimistic: PendingHumanReply = {
      key: `pending:${Date.now()}:${Math.random().toString(36).slice(2, 8)}`,
      text,
      ts: new Date().toISOString(),
    };
    setPendingHumans((prev) => {
      const next = new Map(prev);
      next.set(root, [...(prev.get(root) ?? []), optimistic]);
      return next;
    });
    try {
      const res = await submit.mutateAsync({
        content: text,
        session_id: selectedThread.root_session_id,
      });
      // Stamp the request_id so the panel can suppress the thinking
      // placeholder once the agent's live bubble lands.
      setPendingHumans((prev) => {
        const cur = prev.get(root);
        if (!cur) return prev;
        const next = new Map(prev);
        next.set(
          root,
          cur.map((p) =>
            p.key === optimistic.key
              ? { ...p, requestId: res.request_id }
              : p,
          ),
        );
        return next;
      });
    } catch (e) {
      // Surface the failure by withdrawing the optimistic bubble; the user
      // can re-type and retry.
      setPendingHumans((prev) => {
        const cur = prev.get(root);
        if (!cur) return prev;
        const next = new Map(prev);
        next.set(
          root,
          cur.filter((p) => p.key !== optimistic.key),
        );
        return next;
      });
      throw e;
    }
  };

  // Once history catches up, drop optimistic entries that have been echoed
  // back. Two ways to confirm an echo:
  //   1. A matching human row appears (greedy 1:1 by text — duplicate texts
  //      in flight are paired off in send order so we don't drop both on
  //      the first echo).
  //   2. The worker has already produced an agent reply newer than the
  //      pending's submit time (`max agent_ts > pending_ts`). Catches the
  //      case where text-match drifted (clock skew, an upstream rewrite of
  //      the prompt content) so the placeholder doesn't get stuck after
  //      the answer is on screen.
  useEffect(() => {
    if (!selectedRoot) return;
    setPendingHumans((prev) => {
      const cur = prev.get(selectedRoot);
      if (!cur || cur.length === 0) return prev;
      let lastAgentTs = 0;
      const humans: { key: string; text: string }[] = [];
      for (const m of history) {
        if (m.sender.kind === "human") {
          humans.push({
            key: `${m.session_id}:${m.seq}`,
            text: decodeBody(m.body).text,
          });
        } else if (m.sender.kind === "agent") {
          const ts = new Date(m.created_at).getTime();
          if (ts > lastAgentTs) lastAgentTs = ts;
        }
      }
      const used = new Set<string>();
      const remaining = cur.filter((p) => {
        const match = humans.find((c) => !used.has(c.key) && c.text === p.text);
        if (match) {
          used.add(match.key);
          return false;
        }
        if (lastAgentTs > new Date(p.ts).getTime()) return false;
        return true;
      });
      if (remaining.length === cur.length) return prev;
      const next = new Map(prev);
      if (remaining.length === 0) next.delete(selectedRoot);
      else next.set(selectedRoot, remaining);
      return next;
    });
  }, [history, selectedRoot]);

  const threadPendings = useMemo(
    () => (selectedRoot ? (pendingHumans.get(selectedRoot) ?? []) : []),
    [pendingHumans, selectedRoot],
  );

  const poster = isDemo ? DEMO_HUMAN_POSTER : DEMO_USER;
  const rootMessage = useMemo(() => {
    const first = history[0];
    if (!first) return undefined;
    let text = decodeBody(first.body).text;
    const receiver = first.receiver;
    if (receiver.kind === "agent") {
      const recv = agents.find((a) => a.id === receiver.agent_id);
      if (recv && !text.startsWith(`@${recv.name}`)) {
        text = `@${recv.name} ${text}`;
      }
    }
    return {
      name: poster.name,
      id: poster.id,
      ts: first.created_at,
      text,
    };
  }, [history, agents, poster]);

  return (
    <ChatLayout
      rail={<MenuRail />}
      sidebar={
        <Sidebar
          threads={threads}
          agents={agents}
          selectedChannel={selectedAgentId ? "" : CHANNEL}
          selectedAgentId={selectedAgentId}
          onSelectChannel={() => {
            setSelectedAgentId(null);
            setSelectedRoot(null);
          }}
          onSelectAgent={(id) => {
            setSelectedAgentId(id);
            setSelectedRoot(null);
          }}
          userName={DEMO_USER.name}
        />
      }
      main={
        <>
          <ChannelHeader
            channel={selectedAgent ? `dm/${selectedAgent.name}` : CHANNEL}
            agents={agents}
          />
          <MessageList
            threads={visibleThreads}
            channel={selectedAgent ? `dm/${selectedAgent.name}` : CHANNEL}
            userName={DEMO_USER.name}
            humanPoster={isDemo ? DEMO_HUMAN_POSTER : undefined}
            onOpenThread={(rootId) => {
              setSelectedRoot(rootId);
              setShowPanel(true);
            }}
          />
          <Composer
            agents={agents}
            mode={selectedAgent ? "dm" : "channel"}
            dmAgent={selectedAgent ?? undefined}
            channel={CHANNEL}
            pending={submit.isPending}
            disabled={agentsQ.isLoading && !isDemo}
            onSubmit={onSubmit}
          />
        </>
      }
      panel={
        showPanel ? (
          <ThreadPanel
            channel={CHANNEL}
            thread={selectedThread}
            history={replies}
            agents={agents}
            rootMessage={rootMessage}
            pendingHumans={threadPendings}
            pending={submit.isPending}
            onReply={onThreadReply}
            onClose={() => setShowPanel(false)}
          />
        ) : null
      }
    />
  );
}
```

---

## `src/components/organisms/ThreadPanel.tsx`

```tsx
import { useLayoutEffect, useMemo, useRef, useState } from "react";
import {
  AtSign,
  Bell,
  ChevronDown,
  ChevronRight,
  Paperclip,
  Send,
  Smile,
  X,
} from "lucide-react";
import { Monogram } from "../atoms/Monogram";
import { Spinner } from "../atoms/Spinner";
import { Markdown } from "../molecules/Markdown";
import { ToolCallLine, formatMs } from "../molecules/ToolCallLine";
import { MentionInput } from "../molecules/MentionInput";
import { clockTime } from "../../lib/time";
import { cn, insertAtCaret } from "../../lib/utils";
import type {
  Agent,
  ThreadMessage,
  ThreadSummary,
  ToolCallEntry,
} from "../../types/api";
import { useThreadStreamStore, selectBubbles } from "../../stores/threadStream";
import { DEMO_REPLY_META } from "../../lib/demo";
import { foldHistoryIntoBubbles } from "../../lib/chatBody";
import { renderMentions } from "../../lib/mentions";

function agentRef(agents: Agent[], id: string | null | undefined) {
  if (!id) return null;
  return agents.find((a) => a.id === id) ?? null;
}

type Reply = {
  kind: "agent" | "human";
  key: string;
  agent: Agent | null;
  /** Display name for human rows (the thread poster). */
  humanName?: string;
  humanId?: string;
  ts: string;
  text: string;
  reasoning: string;
  tools: ToolCallEntry[];
  isLive: boolean;
};

export type PendingHumanReply = {
  key: string;
  text: string;
  ts: string;
  /** Set after `/prompts` returns. Lets the panel correlate the optimistic
   *  bubble to the agent's live stream so the thinking placeholder stops
   *  showing the moment the agent starts responding. */
  requestId?: string;
};

export function ThreadPanel({
  channel,
  thread,
  history,
  agents,
  rootMessage,
  pendingHumans,
  pending,
  onReply,
  onClose,
}: {
  channel: string;
  thread: ThreadSummary | null;
  history: ThreadMessage[];
  agents: Agent[];
  rootMessage?: { name: string; id: string; ts: string; text: string };
  /** Optimistic human follow-ups that have been submitted but not yet
   *  echoed back through G2 history. Cleared by the parent once the
   *  matching row appears. */
  pendingHumans?: PendingHumanReply[];
  pending?: boolean;
  onReply?: (input: { content: string }) => void;
  onClose?: () => void;
}) {
  const [reply, setReply] = useState("");
  const replyRef = useRef<HTMLTextAreaElement | null>(null);
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const trimmed = reply.trim();
  const sendReply = () => {
    if (!trimmed || pending || !thread) return;
    onReply?.({ content: trimmed });
    setReply("");
    // Pin to the bottom synchronously after the optimistic bubble has had
    // a chance to render. Two RAFs cover both React commit and the
    // bubble's measured layout.
    requestAnimationFrame(() => {
      requestAnimationFrame(() => {
        const el = scrollRef.current;
        if (el) el.scrollTop = el.scrollHeight;
      });
    });
  };
  const insertAt = () => insertAtCaret(replyRef, reply, setReply, "@");
  const stream = useThreadStreamStore((s) =>
    thread ? s.byThread.get(thread.root_request_id) : null,
  );
  const liveBubbles = stream ? selectBubbles(stream) : null;

  const historyBubbles = useMemo(
    () => foldHistoryIntoBubbles(history, agents),
    [history, agents],
  );

  const replies: Reply[] = useMemo(() => {
    const out: Reply[] = historyBubbles.map((b) => ({
      kind: b.kind,
      key: b.key,
      agent: agentRef(agents, b.agent_id),
      humanName: b.kind === "human" ? rootMessage?.name : undefined,
      humanId: b.kind === "human" ? rootMessage?.id : undefined,
      ts: b.ts,
      text: b.text,
      reasoning: b.reasoning,
      tools: b.tool_calls,
      isLive: false,
    }));
    // Optimistic human follow-ups land between history and live agent
    // bubbles — the user sees their own message echo immediately, the
    // agent's streaming reply appears beneath it.
    if (pendingHumans) {
      for (const p of pendingHumans) {
        out.push({
          kind: "human",
          key: p.key,
          agent: null,
          humanName: rootMessage?.name,
          humanId: rootMessage?.id,
          ts: p.ts,
          text: p.text,
          reasoning: "",
          tools: [],
          isLive: true,
        });
      }
    }
    // Live bubbles tail history. We keep showing them past `done` until
    // the matching agent text shows up in folded history — otherwise the
    // bubble flashes empty in the gap between `done` and the G2 refetch.
    let hasStreamingAgent = false;
    if (liveBubbles) {
      const historyAgentTexts = new Set(
        historyBubbles
          .filter((b) => b.kind === "agent" && b.text.length > 0)
          .map((b) => b.text),
      );
      for (const b of liveBubbles) {
        if (b.status !== "streaming") {
          // Drop only once history has caught up; otherwise hold the bubble
          // visible so the conversation never appears to lose its tail.
          if (b.message && historyAgentTexts.has(b.message)) continue;
        } else {
          hasStreamingAgent = true;
        }
        out.push({
          kind: "agent",
          key: `b:${b.request_id}`,
          agent: agentRef(agents, b.from_agent),
          ts: b.ts,
          text: b.message,
          reasoning: b.reasoning,
          tools: Array.from(b.tool_calls.values()),
          isLive: true,
        });
      }
    }
    // Placeholder while the worker is still composing the first chunk —
    // covers the gap between submit and the model emitting its first
    // chunk, which can be 1–3s on Claude. Suppressed as soon as ANY of:
    //   (a) a live bubble exists for the pending's request_id (agent has
    //       started streaming),
    //   (b) a streaming live bubble exists at all (agent activity in
    //       flight, any request),
    //   (c) the folded history already has an agent bubble newer than
    //       this pending's submit ts (the answer is on screen — common
    //       when SSE missed chunks but G2 caught up).
    const liveRequestIds = new Set(
      (liveBubbles ?? []).map((b) => b.request_id),
    );
    const lastHistoryAgentTs = historyBubbles
      .filter((b) => b.kind === "agent")
      .reduce((m, b) => Math.max(m, new Date(b.ts).getTime()), 0);
    const stillWaiting = (pendingHumans ?? []).some((p) => {
      if (p.requestId && liveRequestIds.has(p.requestId)) return false;
      if (lastHistoryAgentTs > new Date(p.ts).getTime()) return false;
      return true;
    });
    if (stillWaiting && !hasStreamingAgent) {
      out.push({
        kind: "agent",
        key: `thinking:placeholder`,
        agent: null,
        ts: new Date().toISOString(),
        text: "",
        reasoning: "",
        tools: [],
        isLive: true,
      });
    }
    return out;
  }, [historyBubbles, liveBubbles, agents, rootMessage, pendingHumans]);

  // While the conversation grows from incoming chunks/history, follow the
  // tail only if the reader is already near the bottom — don't yank a
  // user who scrolled up to re-read older replies.
  const lastSignature = useRef<string>("");
  const signature = useMemo(() => {
    const tail = replies[replies.length - 1];
    return `${replies.length}|${tail?.key ?? ""}|${tail?.text.length ?? 0}`;
  }, [replies]);
  useLayoutEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    if (signature === lastSignature.current) return;
    const distanceFromBottom =
      el.scrollHeight - el.scrollTop - el.clientHeight;
    const wasAtBottom = distanceFromBottom < 120;
    lastSignature.current = signature;
    if (wasAtBottom) el.scrollTop = el.scrollHeight;
  }, [signature]);

  return (
    <aside
      className="flex h-full w-[360px] shrink-0 flex-col border-l border-[var(--color-line)] bg-[var(--color-paper)]"
      aria-label="Thread side panel"
    >
      <header className="flex items-center justify-between gap-2 border-b border-[var(--color-line)] px-5 py-3">
        <div>
          <div className="font-[var(--font-mono)] text-[10px] uppercase tracking-[0.18em] text-[var(--color-muted)]">
            Thread
          </div>
          <div className="mt-0.5 font-[var(--font-display)] text-[16px] font-bold text-[var(--color-ink)]">
            Replies in <span className="font-[var(--font-mono)]">#{channel}</span>
          </div>
        </div>
        <button
          aria-label="Close"
          onClick={onClose}
          className="flex h-7 w-7 items-center justify-center text-[var(--color-muted)] hover:text-[var(--color-ink)]"
        >
          <X className="h-4 w-4" />
        </button>
      </header>

      <div ref={scrollRef} className="flex-1 overflow-y-auto scroll-thin">
        {/* Root post */}
        {rootMessage && (
          <article className="flex gap-3 border-b border-[var(--color-line)] px-5 py-4">
            <Monogram name={rootMessage.name} id={rootMessage.id} size={28} />
            <div className="min-w-0 flex-1">
              <div className="flex items-baseline gap-2">
                <span className="font-[var(--font-display)] text-[13.5px] font-bold text-[var(--color-ink)]">
                  {rootMessage.name}
                </span>
                <span className="font-[var(--font-mono)] text-[11px] text-[var(--color-muted-2)]">
                  {clockTime(rootMessage.ts)}
                </span>
              </div>
              <p className="mt-0.5 text-[13.5px] leading-[1.5] text-[var(--color-ink)]">
                {renderMentions(rootMessage.text)}
              </p>
            </div>
          </article>
        )}

        {/* Replies count bar */}
        <div className="flex items-center justify-between border-b border-[var(--color-line)] bg-[var(--color-paper-2)] px-5 py-1.5">
          <span className="font-[var(--font-mono)] text-[10px] uppercase tracking-[0.18em] text-[var(--color-muted)]">
            {replies.length} {replies.length === 1 ? "reply" : "replies"}
          </span>
          <button
            aria-label="Notifications"
            className="text-[var(--color-muted)] hover:text-[var(--color-ink)]"
          >
            <Bell className="h-3.5 w-3.5" />
          </button>
        </div>

        <div className="flex flex-col">
          {replies.length === 0 && (
            <p className="px-5 py-6 font-[var(--font-mono)] text-[12px] text-[var(--color-muted-2)]">
              No replies yet.
            </p>
          )}
          {replies.map((r) =>
            r.kind === "human" ? (
              <HumanReplyCard key={r.key} reply={r} />
            ) : (
              <ReplyCard key={r.key} reply={r} />
            ),
          )}
        </div>
      </div>

      {/* Reply composer */}
      <form
        onSubmit={(e) => {
          e.preventDefault();
          sendReply();
        }}
        className="border-t border-[var(--color-line)] bg-[var(--color-paper)] p-3"
      >
        <div
          className={cn(
            "border border-[var(--color-line-strong)] bg-[var(--color-card)] focus-within:ring-2 focus-within:ring-[var(--color-moss)]/15",
            !thread && "opacity-60",
          )}
        >
          <MentionInput
            value={reply}
            onChange={setReply}
            agents={agents}
            mode="thread"
            placeholder="Reply… (use @ to mention an agent)"
            onSubmit={sendReply}
            disabled={!thread || pending}
            textRef={replyRef}
            rows={1}
            maxHeight={140}
          />
          <div className="flex items-center gap-1 border-t border-[var(--color-line)] px-2 py-1">
            <button
              type="button"
              aria-label="Mention"
              onClick={insertAt}
              className="flex h-6 w-6 items-center justify-center text-[var(--color-muted)] hover:bg-[var(--color-paper-2)]"
            >
              <AtSign className="h-3.5 w-3.5" />
            </button>
            <button
              type="button"
              aria-label="Attach"
              className="flex h-6 w-6 items-center justify-center text-[var(--color-muted)] hover:bg-[var(--color-paper-2)]"
            >
              <Paperclip className="h-3.5 w-3.5" />
            </button>
            <button
              type="button"
              aria-label="Emoji"
              className="flex h-6 w-6 items-center justify-center text-[var(--color-muted)] hover:bg-[var(--color-paper-2)]"
            >
              <Smile className="h-3.5 w-3.5" />
            </button>
            <button
              type="submit"
              disabled={!trimmed || pending || !thread}
              className="ml-auto inline-flex h-7 items-center gap-1 bg-[var(--color-moss)] px-2.5 font-[var(--font-mono)] text-[11.5px] font-semibold text-white hover:bg-[var(--color-moss-deep)] disabled:cursor-not-allowed disabled:opacity-40"
            >
              {pending ? (
                <>
                  <Spinner size={10} className="text-white" /> sending
                </>
              ) : (
                <>
                  Send <Send className="h-3 w-3" strokeWidth={2.5} />
                </>
              )}
            </button>
          </div>
        </div>
      </form>
    </aside>
  );
}

function HumanReplyCard({ reply }: { reply: Reply }) {
  const name = reply.humanName ?? "you";
  return (
    <article className="flex gap-3 border-b border-[var(--color-line)] px-5 py-4">
      <Monogram name={name} id={reply.humanId ?? name} size={22} tone="user" />
      <div className="min-w-0 flex-1">
        <header className="flex items-baseline gap-2">
          <span className="font-[var(--font-display)] text-[13px] font-bold text-[var(--color-ink)]">
            {name}
          </span>
          <span className="ml-auto font-[var(--font-mono)] text-[11px] text-[var(--color-muted-2)]">
            {clockTime(reply.ts)}
          </span>
        </header>
        <p className="mt-0.5 text-[13.5px] leading-[1.5] text-[var(--color-ink)]">
          {renderMentions(reply.text)}
        </p>
      </div>
    </article>
  );
}

function ReplyCard({ reply }: { reply: Reply }) {
  // Demo metas pre-populate reasoning + tool calls when the bubble doesn't
  // yet carry them — keeps the design-reference panel honest without
  // coupling demo fixtures to live wire data.
  const meta = DEMO_REPLY_META[reply.key];
  const tools: (ToolCallEntry & { durationMs?: number })[] =
    reply.tools.length > 0
      ? reply.tools
      : meta?.tools.map((t, i) => ({
          call_id: `${reply.key}:${i}`,
          name: t.name,
          input: t.args,
          output: undefined,
          status: "ok" as const,
          durationMs: t.durationMs,
        })) ?? [];
  const reasoning = reply.reasoning || meta?.reasoning || "";
  const tokens = meta?.tokens ?? 0;
  const durationMs = meta?.durationMs ?? 0;
  const hasMeta = tools.length > 0 || reasoning.length > 0 || tokens > 0;

  const [open, setOpen] = useState(meta?.expanded ?? false);

  return (
    <article className="border-b border-[var(--color-line)] px-5 py-4">
      <header className="flex items-center gap-2">
        <Monogram
          name={reply.agent?.name ?? "agent"}
          id={reply.agent?.id ?? "agent"}
          size={22}
          tone="moss"
        />
        <span className="font-[var(--font-display)] text-[13px] font-bold text-[var(--color-ink)]">
          {reply.agent?.name ?? "agent"}
        </span>
        <span className="border border-[var(--color-moss)] px-1 font-[var(--font-mono)] text-[9.5px] font-bold uppercase tracking-[0.14em] text-[var(--color-moss)]">
          AGENT
        </span>
        <span className="ml-auto font-[var(--font-mono)] text-[11px] text-[var(--color-muted-2)]">
          {clockTime(reply.ts)}
        </span>
      </header>

      <div className="mt-1.5 text-[13px] leading-[1.5] text-[var(--color-ink)]">
        {reply.text ? (
          <Markdown text={reply.text} className="text-[13px]" />
        ) : reply.isLive ? (
          <ThinkingIndicator />
        ) : null}
      </div>

      {hasMeta && (
        <button
          onClick={() => setOpen((v) => !v)}
          className="mt-2.5 flex w-full items-center gap-2 border border-[var(--color-line)] bg-[var(--color-paper-2)] px-2.5 py-1 font-[var(--font-mono)] text-[11px] text-[var(--color-muted)] hover:text-[var(--color-ink)] transition-colors"
        >
          {open ? (
            <ChevronDown className="h-3 w-3" />
          ) : (
            <ChevronRight className="h-3 w-3" />
          )}
          <span className="font-medium">reasoning</span>
          <span className="text-[var(--color-line-2)]">|</span>
          <span>{tools.length} tools</span>
          <span className="text-[var(--color-line-2)]">|</span>
          <span>
            {tokens >= 1000 ? `${(tokens / 1000).toFixed(1)}k` : tokens} tok
          </span>
          <span className="text-[var(--color-line-2)]">|</span>
          <span className="ml-auto">{formatMs(durationMs)}</span>
        </button>
      )}

      {open && (
        <div className="mt-2 space-y-3 border border-[var(--color-moss-soft)] bg-[var(--color-moss-tint)] p-3">
          {reasoning && (
            <div>
              <div className="mb-1.5 font-[var(--font-mono)] text-[10px] uppercase tracking-[0.16em] text-[var(--color-moss-deep)]">
                Reasoning
              </div>
              <p className="font-[var(--font-sans)] text-[12px] italic leading-[1.55] text-[var(--color-ink-2)] whitespace-pre-wrap">
                {reasoning}
              </p>
            </div>
          )}
          {tools.length > 0 && (
            <div>
              <div
                className={cn(
                  "mb-1.5 font-[var(--font-mono)] text-[10px] uppercase tracking-[0.16em] text-[var(--color-moss-deep)]",
                  reasoning && "border-t border-[var(--color-moss-soft)] pt-2",
                )}
              >
                Tool Calls
              </div>
              <div className="space-y-1">
                {tools.map((t) => (
                  <ToolCallLine
                    key={t.call_id}
                    call={{
                      call_id: t.call_id,
                      name: t.name,
                      input: t.input,
                      output: t.output,
                      status: t.status,
                    }}
                    durationMs={(t as { durationMs?: number }).durationMs}
                  />
                ))}
              </div>
            </div>
          )}
        </div>
      )}
    </article>
  );
}

function ThinkingIndicator() {
  return (
    <div
      className="inline-flex items-center gap-1.5 font-[var(--font-mono)] text-[11.5px] text-[var(--color-muted)]"
      aria-label="Agent is thinking"
    >
      <span className="flex gap-0.5">
        <span className="h-1 w-1 animate-pulse rounded-full bg-[var(--color-moss)] [animation-delay:0ms]" />
        <span className="h-1 w-1 animate-pulse rounded-full bg-[var(--color-moss)] [animation-delay:150ms]" />
        <span className="h-1 w-1 animate-pulse rounded-full bg-[var(--color-moss)] [animation-delay:300ms]" />
      </span>
      <span>thinking…</span>
    </div>
  );
}
```

---

## `src/lib/chatBody.ts`

```ts
// Decode a `ChatMessage` JSONB envelope into pieces the UI can render.
// Backend wire shape: `{role, contents: [{kind, value}]}` — see
// src/provider/chat.rs.

import type {
  Agent,
  ChatMessageBody,
  ThreadMessage,
  ToolCallEntry,
} from "../types/api";

export type DecodedBody = {
  text: string;
  reasoning: string;
  toolCalls: { id: string; name: string; input: unknown }[];
  toolResults: { call_id: string; output: string; is_error?: boolean }[];
};

export function decodeBody(body: ChatMessageBody | undefined): DecodedBody {
  const out: DecodedBody = {
    text: "",
    reasoning: "",
    toolCalls: [],
    toolResults: [],
  };
  if (!body) return out;

  if (Array.isArray(body.contents)) {
    for (const c of body.contents) {
      switch (c.kind) {
        case "text":
          out.text += (out.text ? "\n" : "") + c.value;
          break;
        case "reasoning":
          out.reasoning += (out.reasoning ? "\n" : "") + c.value;
          break;
        case "tool_call":
          out.toolCalls.push(c.value);
          break;
        case "tool_result":
          out.toolResults.push(c.value);
          break;
      }
    }
    return out;
  }

  if (typeof body.content === "string") out.text = body.content;
  return out;
}

// `send_message` is the only mechanism for human-deliverable content (see
// `src/tools/system/send_message.rs`); plain assistant `text` and the
// matching `tool_result` row are private worker-internals that should not
// surface in the chat panel.
const SEND_MESSAGE = "send_message";

type ReceiverInput =
  | { kind: "human" }
  | { kind: "agent"; agent_id: string };

type SendMessageInput = {
  content: string;
  receiver?: ReceiverInput;
  context_summary?: string;
};

// One bubble per `send_message` invocation, plus one bubble per human
// follow-up row after the root prompt. Reasoning + non-`send_message`
// tool calls observed since the agent's last delivery in this session pile
// under the agent bubble as collapsible meta.
export type HistoryBubble = {
  kind: "agent" | "human";
  key: string;
  agent_id: string | null;
  agent_name: string | null;
  receiver: ReceiverInput | null;
  ts: string;
  text: string;
  reasoning: string;
  tool_calls: ToolCallEntry[];
};

type Pending = {
  reasoning: string;
  tool_calls: ToolCallEntry[];
  // Index for O(1) tool_result lookup by call_id.
  index: Map<string, ToolCallEntry>;
};

const newPending = (): Pending => ({
  reasoning: "",
  tool_calls: [],
  index: new Map(),
});

export function foldHistoryIntoBubbles(
  history: ThreadMessage[],
  agents: Agent[],
): HistoryBubble[] {
  const agentsById = new Map(agents.map((a) => [a.id, a]));
  const bubbles: HistoryBubble[] = [];
  // Per-(session, agent) pending meta — reasoning + non-`send_message`
  // tool calls accumulated since this agent's last delivery in this session.
  const pending = new Map<string, Pending>();
  // Most recent bubble per (session, agent). Reasoning rows that arrive
  // *after* an agent's send_message but before their next one are
  // post-delivery reflection — attach them back to the bubble that just
  // shipped instead of bleeding into the next agent's bubble.
  const lastBubble = new Map<string, HistoryBubble>();
  // Per-session tool index for `tool_result` lookups (results land via
  // `system` rows that don't carry the original caller's identity).
  const indexBySession = new Map<string, Map<string, ToolCallEntry>>();
  // Track every send_message tool call so we can drop its tool_result.
  const sendMessageCallIds = new Set<string>();
  // The first human row in a thread is the root post (rendered separately
  // in the panel header); only follow-up human rows become bubbles.
  let seenFirstHuman = false;

  const key = (session: string, agent: string | null) =>
    `${session}|${agent ?? ""}`;
  const getPending = (k: string): Pending => {
    let p = pending.get(k);
    if (!p) {
      p = newPending();
      pending.set(k, p);
    }
    return p;
  };
  const getIndex = (sid: string): Map<string, ToolCallEntry> => {
    let i = indexBySession.get(sid);
    if (!i) {
      i = new Map();
      indexBySession.set(sid, i);
    }
    return i;
  };

  for (const m of history) {
    const decoded = decodeBody(m.body);

    if (m.sender.kind === "agent") {
      const aid = m.sender.agent_id ?? null;
      const k = key(m.session_id, aid);
      const p = getPending(k);
      const idx = getIndex(m.session_id);

      const sendCalls = decoded.toolCalls.filter((tc) => tc.name === SEND_MESSAGE);
      const realCalls = decoded.toolCalls.filter((tc) => tc.name !== SEND_MESSAGE);

      for (const tc of realCalls) {
        const entry: ToolCallEntry = {
          call_id: tc.id,
          name: tc.name,
          input: tc.input,
          status: "running",
        };
        p.tool_calls.push(entry);
        p.index.set(tc.id, entry);
        idx.set(tc.id, entry);
      }

      if (sendCalls.length > 0) {
        // Flush: this row delivers the agent's accumulated work. Its own
        // reasoning is part of the same turn that produced the send_message,
        // so it joins pending.reasoning in the new bubble.
        const reasoning = joinText(p.reasoning, decoded.reasoning);
        const tools = p.tool_calls;
        for (const tc of sendCalls) {
          sendMessageCallIds.add(tc.id);
          const input = (tc.input ?? {}) as SendMessageInput;
          const recv = input.receiver ?? null;
          const a = aid ? (agentsById.get(aid) ?? null) : null;
          const bubble: HistoryBubble = {
            kind: "agent",
            key: `h:${m.session_id}:${m.seq}:${tc.id}`,
            agent_id: aid,
            agent_name: a?.name ?? null,
            receiver: recv,
            ts: m.created_at,
            text: prefixWithReceiver(input.content ?? "", recv, agentsById),
            reasoning,
            tool_calls: tools,
          };
          bubbles.push(bubble);
          lastBubble.set(k, bubble);
        }
        // Pending fully consumed.
        pending.set(k, newPending());
      } else if (decoded.reasoning) {
        // No send in this row. If the agent already shipped a bubble in this
        // session, the reasoning is post-delivery reflection — attach it back
        // to that bubble. Otherwise it leads into the next send_message and
        // belongs in pending.
        const lb = lastBubble.get(k);
        if (lb && p.tool_calls.length === 0) {
          lb.reasoning = joinText(lb.reasoning, decoded.reasoning);
        } else {
          p.reasoning = joinText(p.reasoning, decoded.reasoning);
        }
      }
      // Plain assistant `text` is private (see `send_message.rs` top-of-file).

      // Inline tool_results (rare — results normally arrive via a system row).
      attachResults(idx, decoded.toolResults, sendMessageCallIds);
    } else if (m.sender.kind === "system") {
      const idx = indexBySession.get(m.session_id);
      if (idx) {
        attachResults(idx, decoded.toolResults, sendMessageCallIds);
      }
    } else if (m.sender.kind === "human") {
      // The first human row is the thread root post (rendered in the panel
      // header). Subsequent human rows are follow-ups in the same thread —
      // surface them as bubbles so the conversation isn't truncated at the
      // first message.
      if (!seenFirstHuman) {
        seenFirstHuman = true;
      } else if (decoded.text) {
        const recv =
          m.receiver.kind === "agent"
            ? { kind: "agent" as const, agent_id: m.receiver.agent_id }
            : null;
        bubbles.push({
          kind: "human",
          key: `h:${m.session_id}:${m.seq}:user`,
          agent_id: null,
          agent_name: null,
          receiver: recv,
          ts: m.created_at,
          text: prefixWithReceiver(decoded.text, recv, agentsById),
          reasoning: "",
          tool_calls: [],
        });
      }
    }
  }

  return bubbles;
}

function attachResults(
  idx: Map<string, ToolCallEntry>,
  results: { call_id: string; output: string; is_error?: boolean }[],
  drop: Set<string>,
): void {
  for (const tr of results) {
    if (drop.has(tr.call_id)) continue;
    const e = idx.get(tr.call_id);
    if (!e) continue;
    e.output = tr.output;
    e.is_error = tr.is_error;
    e.status = tr.is_error ? "error" : "ok";
  }
}

function prefixWithReceiver(
  content: string,
  receiver: ReceiverInput | null,
  agentsById: ReadonlyMap<string, Agent>,
): string {
  if (!receiver) return content;
  if (receiver.kind === "human") return content;
  const name = agentsById.get(receiver.agent_id)?.name;
  if (!name) return content;
  if (content.startsWith(`@${name}`)) return content;
  return `@${name} ${content}`;
}

function joinText(a: string, b: string): string {
  if (!a) return b;
  if (!b) return a;
  return `${a}\n${b}`;
}
```

---

## `src/hooks/useThreadStream.ts`

```ts
import { useEffect, useRef } from "react";
import { useQueryClient } from "@tanstack/react-query";
import type { ThreadStreamEnvelope } from "../types/api";
import { useThreadStreamStore } from "../stores/threadStream";

/**
 * Open a single SSE connection to G3 for the active thread. Chunks are
 * pushed into the Zustand store, where they are deduped by
 * `(request_id, chunk_seq)` so reconnects + G2 backfill never double-render.
 */
export function useThreadStream(rootId: string | null) {
  const setStatus = useThreadStreamStore((s) => s.setStatus);
  const applyEnvelope = useThreadStreamStore((s) => s.applyEnvelope);
  const qc = useQueryClient();
  const seenRequests = useRef<Set<string>>(new Set());

  useEffect(() => {
    if (!rootId) return;
    seenRequests.current = new Set();
    setStatus(rootId, "connecting");

    const url = `/threads/${rootId}/stream`;
    const es = new EventSource(url);
    let closed = false;

    es.onopen = () => {
      if (!closed) setStatus(rootId, "open");
    };
    es.onerror = () => {
      // Browsers auto-reconnect; reflect the gap in UI.
      if (!closed) setStatus(rootId, "stalled");
    };

    const handle = (e: MessageEvent) => {
      // Bun dev proxy occasionally surfaces empty/keepalive frames as
      // `data: undefined`; skip silently rather than logging.
      if (!e.data || e.data === "undefined") return;
      try {
        const env = JSON.parse(e.data) as ThreadStreamEnvelope;
        applyEnvelope(rootId, env);
        // First chunk for a previously-unseen request_id means the worker
        // has appended the human prompt row that triggered the run; refetch
        // G2 so the user's follow-up message renders alongside the reply
        // instead of waiting for `done`.
        const rid = env.request_id;
        if (rid && !seenRequests.current.has(rid)) {
          seenRequests.current.add(rid);
          qc.invalidateQueries({ queryKey: ["threads", rootId, "messages"] });
        }
        // Quiescent / failure / lag terminal events: G2 has the persisted
        // turn now, so refetch and let the history bubbles take over from
        // the in-memory live ones.
        const k = env.chunk?.kind;
        if (k === "done" || k === "error" || k === "stalled") {
          qc.invalidateQueries({ queryKey: ["threads", rootId, "messages"] });
          qc.invalidateQueries({ queryKey: ["threads"] });
        }
      } catch (err) {
        console.warn("thread.stream.parse_error", err);
      }
    };

    // Backend tags every event with `event:` set to the chunk kind. Listen
    // generically — handler logic is in the store, not per-kind.
    const kinds = [
      "text",
      "reasoning",
      "tool_call",
      "tool_result",
      "agent_message",
      "done",
      "error",
      "stalled",
    ] as const;
    for (const k of kinds) es.addEventListener(k, handle);
    es.addEventListener("message", handle);

    return () => {
      closed = true;
      for (const k of kinds) es.removeEventListener(k, handle);
      es.removeEventListener("message", handle);
      es.close();
      setStatus(rootId, "closed");
    };
  }, [rootId, setStatus, applyEnvelope, qc]);
}
```

---

## `src/hooks/useThreads.ts`

```ts
import { useQuery } from "@tanstack/react-query";
import { api } from "../lib/api";

export function useThreads() {
  return useQuery({
    queryKey: ["threads"],
    queryFn: api.threads,
    refetchInterval: 15_000,
    refetchOnWindowFocus: true,
  });
}

/**
 * G2 thread history.
 *
 * `pollMs` is a fallback for when SSE chunks were dropped (the bun dev
 * proxy occasionally truncates the stream with
 * `ERR_INCOMPLETE_CHUNKED_ENCODING`, and EventSource auto-reconnects do
 * not replay backlog). Pass a small interval while a submission is in
 * flight so the UI eventually catches up to the persisted reply even if
 * the live tap missed every chunk; pass `false` once the conversation
 * has quiesced so the panel isn't polling forever.
 */
export function useThreadHistory(
  rootId: string | null,
  pollMs: number | false = false,
) {
  return useQuery({
    queryKey: ["threads", rootId, "messages"],
    queryFn: () => api.threadMessages(rootId!),
    enabled: !!rootId,
    staleTime: 5_000,
    refetchInterval: pollMs,
  });
}
```

---

## `src/hooks/useSubmitPrompt.ts`

```ts
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { api } from "../lib/api";
import { uuidv7 } from "../lib/utils";

type Vars = {
  session_id?: string;
  agent_id?: string;
  content: string;
};

export function useSubmitPrompt() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (v: Vars) =>
      api.submitPrompt({
        ...v,
        idempotency_key: uuidv7(),
      }),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["threads"] });
    },
  });
}
```

---

## `src/stores/threadStream.ts`

```ts
import { create } from "zustand";
import type {
  ResponseChunk,
  ThreadStreamEnvelope,
  ToolCallEntry,
} from "../types/api";

export type BubbleStatus =
  | "streaming"
  | "done"
  | "error"
  | "stalled";

export type Bubble = {
  request_id: string;
  from_agent: string | null;
  first_seq: number;
  message: string;
  reasoning: string;
  tool_calls: Map<string, ToolCallEntry>;
  status: BubbleStatus;
  error?: string;
  final_text?: string;
  order: number;
  /** Captured when the bubble is first observed; stable across renders. */
  ts: string;
};

export type ThreadStreamState = {
  bubbles: Map<string, Bubble>;
  /** Insertion-ordered view of `bubbles` — selectors read this directly. */
  ordered: Bubble[];
  /** dedup key set: `${request_id}:${seq}` */
  seen: Set<string>;
  status: "idle" | "connecting" | "open" | "stalled" | "closed" | "error";
  cursor: number;
};

type Store = {
  byThread: Map<string, ThreadStreamState>;

  ensure: (rootId: string) => ThreadStreamState;
  setStatus: (rootId: string, status: ThreadStreamState["status"]) => void;
  applyEnvelope: (rootId: string, env: ThreadStreamEnvelope) => void;
  reset: (rootId: string) => void;
};

const emptyState = (): ThreadStreamState => ({
  bubbles: new Map(),
  ordered: [],
  seen: new Set(),
  status: "idle",
  cursor: 0,
});

function applyChunk(b: Bubble, chunk: ResponseChunk): Bubble {
  switch (chunk.kind) {
    case "text":
      return b;
    case "reasoning":
      return { ...b, reasoning: b.reasoning + chunk.value };
    case "agent_message":
      return {
        ...b,
        message: b.message + chunk.content,
        from_agent: b.from_agent ?? chunk.from,
      };
    case "tool_call": {
      const tool_calls = new Map(b.tool_calls);
      tool_calls.set(chunk.id, {
        call_id: chunk.id,
        name: chunk.name,
        input: chunk.input,
        status: "running",
      });
      return { ...b, tool_calls };
    }
    case "tool_result": {
      const existing = b.tool_calls.get(chunk.call_id);
      const tool_calls = new Map(b.tool_calls);
      tool_calls.set(chunk.call_id, {
        call_id: chunk.call_id,
        name: existing?.name ?? "(tool)",
        input: existing?.input,
        output: chunk.output,
        is_error: chunk.is_error,
        status: chunk.is_error ? "error" : "ok",
      });
      return { ...b, tool_calls };
    }
    case "done":
      return { ...b, status: "done", final_text: chunk.final_text };
    case "error":
      return { ...b, status: "error", error: chunk.reason };
    case "stalled":
      return { ...b, status: "stalled" };
  }
}

export const useThreadStreamStore = create<Store>((set, get) => ({
  byThread: new Map(),

  ensure(rootId) {
    const cur = get().byThread.get(rootId);
    if (cur) return cur;
    const next = emptyState();
    const map = new Map(get().byThread);
    map.set(rootId, next);
    set({ byThread: map });
    return next;
  },

  setStatus(rootId, status) {
    set((s) => {
      const cur = s.byThread.get(rootId);
      if (cur && cur.status === status) return s;
      const map = new Map(s.byThread);
      map.set(rootId, { ...(cur ?? emptyState()), status });
      return { byThread: map };
    });
  },

  applyEnvelope(rootId, env) {
    set((s) => {
      const cur = s.byThread.get(rootId) ?? emptyState();
      // Synthetic envelopes (Stalled / Error from fan-in) carry a null
      // request_id; reflect them only on connection status.
      if (env.request_id == null || env.chunk_seq == null) {
        const nextStatus =
          env.chunk.kind === "stalled"
            ? "stalled"
            : env.chunk.kind === "error"
              ? "error"
              : cur.status;
        if (nextStatus === cur.status) return s;
        const map = new Map(s.byThread);
        map.set(rootId, { ...cur, status: nextStatus });
        return { byThread: map };
      }
      const key = `${env.request_id}:${env.chunk_seq}`;
      if (cur.seen.has(key)) return s;

      const existing = cur.bubbles.get(env.request_id);
      const base: Bubble = existing
        ? env.from_agent && !existing.from_agent
          ? { ...existing, from_agent: env.from_agent }
          : existing
        : {
            request_id: env.request_id,
            from_agent: env.from_agent,
            first_seq: env.chunk_seq,
            message: "",
            reasoning: "",
            tool_calls: new Map(),
            status: "streaming",
            order: cur.cursor,
            ts: new Date().toISOString(),
          };
      const updated = applyChunk(base, env.chunk);

      const bubbles = new Map(cur.bubbles);
      bubbles.set(env.request_id, updated);
      const ordered = existing
        ? cur.ordered.map((b) =>
            b.request_id === env.request_id ? updated : b,
          )
        : [...cur.ordered, updated];
      const seen = new Set(cur.seen);
      seen.add(key);

      const next: ThreadStreamState = {
        ...cur,
        bubbles,
        ordered,
        seen,
        cursor: existing ? cur.cursor : cur.cursor + 1,
      };
      const map = new Map(s.byThread);
      map.set(rootId, next);
      return { byThread: map };
    });
  },

  reset(rootId) {
    set((s) => {
      if (!s.byThread.has(rootId)) return s;
      const map = new Map(s.byThread);
      map.set(rootId, emptyState());
      return { byThread: map };
    });
  },
}));

export function selectBubbles(state: ThreadStreamState): Bubble[] {
  return state.ordered;
}
```

---

## `src/types/api.ts`

```ts
// Wire types mirror src/runtime/response.rs and the route handlers.
// Keep in sync with: src/http/routes/threads.rs, src/runtime/response.rs.

export type AgentRef = { id: string; name: string };

export type Agent = {
  id: string;
  name: string;
  is_default: boolean;
};

export type RequestStatus = "pending" | "processing" | "done" | "failed";

export type ThreadSummary = {
  root_request_id: string;
  root_session_id: string;
  first_agent: AgentRef;
  preview: string;
  reply_count: number;
  last_activity_at: string;
  status: RequestStatus;
  created_at: string;
};

export type Participant =
  | { kind: "human" }
  | { kind: "agent"; agent_id: string }
  | { kind: "system" };

// Mirrors src/provider/chat.rs `ChatMessage` + UserContent / AssistantContent.
// Wire shape is `{role, contents: [{kind, value}]}`; the demo fixtures tolerate
// the legacy `{role, content: string}` form too.
export type ContentBlock =
  | { kind: "text"; value: string }
  | { kind: "reasoning"; value: string }
  | {
      kind: "tool_call";
      value: { id: string; name: string; input: unknown };
    }
  | {
      kind: "tool_result";
      value: { call_id: string; output: string; is_error?: boolean };
    };

export type ChatMessageBody = {
  role?: "user" | "assistant" | "system" | "tool";
  contents?: ContentBlock[];
  /** Legacy / demo shorthand. */
  content?: string;
  [k: string]: unknown;
};

export type ThreadMessage = {
  session_id: string;
  seq: number;
  sender: Participant;
  receiver: Participant;
  body: ChatMessageBody;
  created_at: string;
};

// ─── ResponseChunk wire shapes ──────────────────────────────────────────

export type ToolCallPayload = {
  id: string;
  name: string;
  input: unknown;
};

export type ToolResultPayload = {
  call_id: string;
  output: string;
  is_error?: boolean;
};

export type ResponseChunk =
  | { kind: "text"; value: string }
  | { kind: "reasoning"; value: string }
  | { kind: "tool_call"; id: string; name: string; input: unknown }
  | { kind: "tool_result"; call_id: string; output: string; is_error?: boolean }
  | { kind: "agent_message"; from: string; content: string }
  | { kind: "done"; final_text: string }
  | { kind: "error"; reason: string }
  | { kind: "stalled" };

export type ToolCallEntry = {
  call_id: string;
  name: string;
  input?: unknown;
  output?: string;
  is_error?: boolean;
  status: "running" | "ok" | "error";
};

export type ThreadStreamEnvelope = {
  request_id: string | null;
  from_agent: string | null;
  chunk_seq: number | null;
  chunk: ResponseChunk;
};

export type SubmitPromptResponse = {
  request_id: string;
  session_id: string;
  status: RequestStatus;
};
```

---

## `src/lib/api.ts`

```ts
import type {
  Agent,
  SubmitPromptResponse,
  ThreadMessage,
  ThreadSummary,
} from "../types/api";

async function jsonOk<T>(res: Response): Promise<T> {
  if (!res.ok) {
    const body = await res.text().catch(() => "");
    throw new Error(`HTTP ${res.status} ${res.statusText}: ${body}`);
  }
  return res.json() as Promise<T>;
}

export const api = {
  agents: () => fetch("/agents").then(jsonOk<Agent[]>),

  threads: () => fetch("/threads").then(jsonOk<ThreadSummary[]>),

  threadMessages: (rootId: string) =>
    fetch(`/threads/${rootId}/messages`).then(jsonOk<ThreadMessage[]>),

  submitPrompt: (input: {
    session_id?: string;
    agent_id?: string;
    content: string;
    idempotency_key: string;
  }) =>
    fetch("/prompts", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(input),
    }).then(jsonOk<SubmitPromptResponse>),

  cancelRequest: (requestId: string) =>
    fetch(`/requests/${requestId}/cancel`, { method: "POST" }).then((r) => {
      if (!r.ok && r.status !== 404)
        throw new Error(`HTTP ${r.status} ${r.statusText}`);
    }),
};
```
