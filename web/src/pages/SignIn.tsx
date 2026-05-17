import { useMemo } from "react";
import { useLocation } from "react-router-dom";
import { AlertTriangle } from "lucide-react";
import { Banner } from "../components/molecules/Banner";
import { Button } from "../components/atoms/Button";

export function SignIn() {
  const location = useLocation();

  const oauthDown = useMemo(() => {
    const params = new URLSearchParams(location.search);
    return params.get("error") === "oauth_unavailable";
  }, [location.search]);

  const returnTo = useMemo(() => {
    const params = new URLSearchParams(location.search);
    const fromQuery = params.get("from");
    if (fromQuery && fromQuery !== "/sign-in") return fromQuery;
    const state = location.state as { from?: string } | null;
    if (state?.from && state.from !== "/sign-in") return state.from;
    return "/";
  }, [location.search, location.state]);

  const onSignIn = () => {
    window.location.href = `/auth/google/login?return_to=${encodeURIComponent(returnTo)}`;
  };

  return (
    <div className="grain-paper flex h-screen w-screen items-center justify-center bg-[var(--color-paper)]">
      <div className="w-[420px] max-w-[90vw] border border-[var(--color-line)] bg-[var(--color-card)] px-8 py-9 shadow-sm">
        <div className="font-[var(--font-mono)] text-[10.5px] uppercase tracking-[0.22em] text-[var(--color-muted)]">
          Relay
        </div>
        <h1 className="mt-1 font-[var(--font-display)] text-[28px] font-bold leading-[1.15] tracking-tight text-[var(--color-ink)]">
          Sign in
        </h1>
        <p className="mt-3 text-[13px] leading-[1.55] text-[var(--color-muted)]">
          Continue with Google to enter your workspace.
        </p>

        {oauthDown ? (
          <div className="mt-5">
            <Banner
              variant="amber"
              icon={<AlertTriangle className="h-3.5 w-3.5" />}
            >
              Google is unreachable, try again.
            </Banner>
          </div>
        ) : null}

        <div className="mt-7">
          <Button
            variant="primary"
            size="md"
            onClick={onSignIn}
            className="w-full"
          >
            Sign in with Google
          </Button>
        </div>
      </div>
    </div>
  );
}
