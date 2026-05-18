export class ApiError extends Error {
  readonly status: number;
  readonly body: string;
  constructor(status: number, body: string) {
    super(`HTTP ${status}: ${body || "(no body)"}`);
    this.name = "ApiError";
    this.status = status;
    this.body = body;
  }
}

export class AuthRedirect extends Error {
  constructor() {
    super("AuthRedirect");
    this.name = "AuthRedirect";
  }
}

export function formatError(err: unknown): string {
  if (err instanceof ApiError) return err.body || `HTTP ${err.status}`;
  if (err instanceof Error) return err.message;
  return "Unexpected error";
}
