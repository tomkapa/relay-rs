import { useEffect } from "react";
import { useQuery } from "@tanstack/react-query";
import { api } from "../lib/api";
import { useAuthStore } from "../stores/authStore";
import type { Me, Org } from "../types/api";

export const ME_QUERY_KEY = ["me"] as const;

export function useMe() {
  const query = useQuery<Me>({
    queryKey: ME_QUERY_KEY,
    queryFn: api.me,
    retry: false,
    staleTime: 60_000,
  });

  useEffect(() => {
    if (!query.data) return;
    const { me, setMe } = useAuthStore.getState();
    if (me !== query.data) setMe(query.data);
  }, [query.data]);

  return query;
}

export function useActiveOrg(): Org | null {
  const me = useAuthStore((s) => s.me);
  if (!me) return null;
  return me.orgs.find((o) => o.id === me.active_org_id) ?? null;
}
