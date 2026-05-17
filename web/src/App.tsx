import { Route, Routes } from "react-router-dom";
import { ChatView } from "./pages/ChatView";
import { SignIn } from "./pages/SignIn";
import { Protected } from "./components/organisms/Protected";

export function App() {
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
