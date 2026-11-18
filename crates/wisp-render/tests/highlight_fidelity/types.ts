import type { Load } from './$types';
export interface Page<T = unknown> { data: T; status?: number }
export const load: Load = async ({ params, fetch }) => {
  const res = await fetch(`/api/${params.id}?q=${encodeURIComponent("a b")}`);
  return { data: (await res.json()) as Page<string>, status: 200 };
};
