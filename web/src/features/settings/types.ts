/** Fields from GET/PUT /api/settings. */
export type SettingsDto = {
  socialEnabled: boolean;
};

/** Row from GET /api/admin/sessions. */
export type AdminSessionDto = {
  token: string;
  tokenPreview: string;
  userId: number;
  expiresAt: string;
  createdAt: string;
  current: boolean;
};
