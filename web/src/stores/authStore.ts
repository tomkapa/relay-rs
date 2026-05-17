import { create } from "zustand";
import type { Me } from "../types/api";

export type AuthError =
  | { kind: "forbidden"; message?: string }
  | { kind: "oauth_down" };

type AuthStore = {
  me: Me | null;
  error: AuthError | null;
  setMe: (me: Me | null) => void;
  clearMe: () => void;
  setError: (err: AuthError) => void;
  clearError: () => void;
};

export const useAuthStore = create<AuthStore>((set) => ({
  me: null,
  error: null,
  setMe: (me) => set({ me }),
  clearMe: () => set({ me: null }),
  setError: (err) => set({ error: err }),
  clearError: () => set({ error: null }),
}));
