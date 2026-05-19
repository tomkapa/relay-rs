import { Route, Routes } from "react-router-dom";
import { AgentDetail } from "./pages/AgentDetail";
import { ChatView } from "./pages/ChatView";
import { ConnectionDetail } from "./pages/ConnectionDetail";
import { ConnectionsCatalog } from "./pages/ConnectionsCatalog";
import { ConnectionsList } from "./pages/ConnectionsList";
import { OAuthCallback } from "./pages/OAuthCallback";
import { SignIn } from "./pages/SignIn";
import { Protected } from "./components/organisms/Protected";
import { useLangFromOrg } from "./i18n";

export function App() {
  // Subscribe once at the root so the active org's language is mirrored
  // into the i18n module on initial load and on every org switch.
  // Pre-auth (no `me` yet) the hook is a no-op and `t()` keeps using the
  // browser-detected default.
  useLangFromOrg();
  return (
    <Routes>
      <Route path="/sign-in" element={<SignIn />} />
      <Route
        path="/connections"
        element={
          <Protected>
            <ConnectionsList />
          </Protected>
        }
      />
      <Route
        path="/connections/catalog"
        element={
          <Protected>
            <ConnectionsCatalog />
          </Protected>
        }
      />
      <Route
        path="/connections/oauth-callback"
        element={
          <Protected>
            <OAuthCallback />
          </Protected>
        }
      />
      <Route
        path="/connections/:id"
        element={
          <Protected>
            <ConnectionDetail />
          </Protected>
        }
      />
      <Route
        path="/agents/:id"
        element={
          <Protected>
            <AgentDetail />
          </Protected>
        }
      />
      <Route
        path="/*"
        element={
          <Protected>
            <ChatView />
          </Protected>
        }
      />
    </Routes>
  );
}
