export class ApiError extends Error {
  constructor(message: string, public status: number) { super(message); }
}
export async function api<T>(path: string, method = 'GET', body?: unknown, signal?: AbortSignal): Promise<T> {
  const response = await fetch('/api' + path, {
    method, credentials: 'same-origin', signal,
    headers: { 'Content-Type': 'application/json', 'X-Chain-Request': '1' },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await response.text();
  let result;
  try { result = JSON.parse(text); }
  catch { throw new ApiError(`服务器返回了无法读取的响应 (${response.status})`, response.status); }
  if (!response.ok) {
    if (response.status === 401 && path !== '/login') window.dispatchEvent(new Event('session-expired'));
    throw new ApiError(result.error || `请求失败 (${response.status})`, response.status);
  }
  return result;
}
