export type AuthContextValue = {
  token: string;
  sessionExpiresAt: string;
  isAuthenticated: boolean;
  busy: boolean;
  err: string;
  setErr: (s: string) => void;
  applySecretToken: (s: string) => void;
  applySessionToken: (token: string, expiresAt?: string) => void;
  clearAuth: () => void;
  logout: () => void;
  loginWithPasswordHttp: (p: {
    username: string;
    password: string;
  }) => Promise<{ token: string; expiresAt: string }>;
  bootstrapHttp: (p: {
    adminSecret: string;
    loginUser: string;
    password: string;
  }) => Promise<{ token: string; expiresAt: string }>;
};
