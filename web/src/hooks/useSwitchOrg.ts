import { useMutation, useQueryClient } from "@tanstack/react-query";
import { api } from "../lib/api";

export function useSwitchOrg() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (orgId: string) => api.switchOrg(orgId),
    onSuccess: () => qc.invalidateQueries(),
  });
}
