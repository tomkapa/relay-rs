import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "react-router-dom";
import { api } from "../lib/api";
import { useAuthStore } from "../stores/authStore";

export function useLogout() {
  const qc = useQueryClient();
  const navigate = useNavigate();
  const clearMe = useAuthStore((s) => s.clearMe);
  return useMutation({
    mutationFn: api.logout,
    onSuccess: () => {
      qc.clear();
      clearMe();
      navigate("/sign-in", { replace: true });
    },
  });
}
