import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider } from "@tanstack/react-router";

import { Toast, ToastList } from "@/components/ui/toast";
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
  return <RouterProvider router={router} context={{ auth, queryClient }} />;
}

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <div className="root">
      <QueryClientProvider client={queryClient}>
        <Toast.Provider>
          <AuthProvider>
            <InnerApp />
          </AuthProvider>
          <Toast.Portal>
            <Toast.Viewport>
              <ToastList />
            </Toast.Viewport>
          </Toast.Portal>
        </Toast.Provider>
      </QueryClientProvider>
    </div>
  </StrictMode>,
);
