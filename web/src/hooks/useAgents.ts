import {
  useInfiniteQuery,
  useMutation,
  useQuery,
  useQueryClient,
} from "@tanstack/react-query";
import { api } from "../lib/api";
import { ApiError } from "../lib/errors";
import type { UpdateAgentRequest } from "../types/api";

const LIST_KEY = ["agents"] as const;
const ONE_KEY = (id: string) => ["agents", id] as const;
const TOOL_CALLS_KEY = (id: string) => ["agents", id, "tool-calls"] as const;

export function useAgents() {
  return useQuery({
    queryKey: LIST_KEY,
    queryFn: api.agents,
    staleTime: 60_000,
  });
}

export function useAgent(
  id: string | null,
  options?: { refetchInterval?: number | false; enabled?: boolean },
) {
  return useQuery({
    queryKey: id ? ONE_KEY(id) : ["agents", "none"],
    queryFn: () => api.agent(id ?? ""),
    enabled: Boolean(id) && options?.enabled !== false,
    refetchInterval: options?.refetchInterval,
    staleTime: 0,
    retry: (count, err) => {
      if (
        err instanceof ApiError &&
        (err.status === 404 || err.status === 403)
      ) {
        return false;
      }
      return count < 3;
    },
  });
}

export function useUpdateAgent() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ id, patch }: { id: string; patch: UpdateAgentRequest }) =>
      api.updateAgent(id, patch),
    onSuccess: (_, vars) => {
      qc.invalidateQueries({ queryKey: LIST_KEY });
      qc.invalidateQueries({ queryKey: ONE_KEY(vars.id) });
    },
  });
}

/** Cursor-paginated audit list for a single agent. Mirrors
 *  `useMcpServerToolCalls` — the page card uses `flatMap(pages, p.items)`
 *  and `fetchNextPage()` for "load more". */
export function useAgentToolCalls(id: string | null) {
  return useInfiniteQuery({
    queryKey: id ? TOOL_CALLS_KEY(id) : ["agents", "none", "tool-calls"],
    enabled: Boolean(id),
    initialPageParam: undefined as string | undefined,
    queryFn: ({ pageParam }) =>
      api.agentToolCalls(id ?? "", { before: pageParam }),
    getNextPageParam: (last) => last.next_cursor ?? undefined,
    // Live updates: the activity card sits visible while the operator
    // edits the allowlist; 15s matches RecentActivityCard.
    refetchInterval: 15_000,
    staleTime: 0,
  });
}
