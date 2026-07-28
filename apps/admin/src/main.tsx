import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider } from "@tanstack/react-router";

import { AuthProvider, useAuth } from "@/features/auth/auth-context";
import { createAppQueryClient } from "@/lib/query-client";
import { endAdminSession } from "@/lib/session-end-app";
import { router } from "@/router";

import "./styles.css";

const queryClient = createAppQueryClient({
  onUnauthorized: () => endAdminSession(queryClient),
});

function InnerApp() {
  const auth = useAuth();
  return (
    <RouterProvider router={router} context={{ auth, queryClient }} />
  );
}

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <QueryClientProvider client={queryClient}>
      <AuthProvider>
        <InnerApp />
      </AuthProvider>
    </QueryClientProvider>
  </StrictMode>,
);
