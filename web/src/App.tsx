import { Route, Routes } from "react-router-dom";
import { ChatView } from "./pages/ChatView";
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
