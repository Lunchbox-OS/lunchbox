// Decides between the login page and the application (issue #156).
//
// One question on load — `GET /api/v1/auth/session` — rather than letting the
// first data fetch fail and inferring a login from the 401. The difference
// matters: a page that renders its dashboard, fires six queries and then
// replaces itself with a login form is a page that flashed the shape of the
// device at somebody who is not signed in.
//
// The gate also listens for a 401 arriving later, because a session can expire
// or be revoked from another device while this tab is open. `client.ts` raises
// `shepherd:unauthenticated` on any 401 and this is what catches it.

import { useCallback, useEffect, useState } from "react";
import Box from "@mui/material/Box";
import CircularProgress from "@mui/material/CircularProgress";
import { getAuthStatus, whoAmI, type WebAuthStatus } from "../api/auth";
import { LoginPage } from "../pages/LoginPage";

/** Raised by the API client whenever the device answers 401. */
export const UNAUTHENTICATED_EVENT = "shepherd:unauthenticated";

type State =
  | { phase: "checking" }
  | { phase: "signed-in" }
  | { phase: "signed-out"; status: WebAuthStatus };

export function AuthGate({ children }: { children: React.ReactNode }) {
  const [state, setState] = useState<State>({ phase: "checking" });

  const check = useCallback(async () => {
    try {
      await whoAmI();
      setState({ phase: "signed-in" });
    } catch {
      try {
        setState({ phase: "signed-out", status: await getAuthStatus() });
      } catch {
        // The device is unreachable rather than refusing us. Offer the login
        // form anyway: it is the only screen that works without data, and its
        // error message will be the honest one.
        setState({
          phase: "signed-out",
          status: { configured: true, companion_available: false },
        });
      }
    }
  }, []);

  useEffect(() => {
    const onUnauthenticated = () => setState({ phase: "checking" });
    window.addEventListener(UNAUTHENTICATED_EVENT, onUnauthenticated);
    return () => window.removeEventListener(UNAUTHENTICATED_EVENT, onUnauthenticated);
  }, []);

  // One effect, driven by the phase: it runs on mount (the initial phase is
  // "checking") and again whenever a later 401 puts us back there. Two effects
  // — one for mount, one for the phase — would check twice on load.
  useEffect(() => {
    if (state.phase === "checking") void check();
  }, [state.phase, check]);

  if (state.phase === "checking") {
    return (
      <Box
        sx={{
          minHeight: "100dvh",
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
        }}
      >
        <CircularProgress />
      </Box>
    );
  }

  if (state.phase === "signed-out") {
    return (
      <LoginPage
        status={state.status}
        onSignedIn={() => setState({ phase: "signed-in" })}
      />
    );
  }

  return <>{children}</>;
}
