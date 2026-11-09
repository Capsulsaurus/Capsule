// The proxy both Pages Functions share: forward the request to the origin
// `proxyTarget` names (an old version's branch deployment, or GitHub Pages for
// rustdoc), and keep any redirect it answers with on the docs domain. The URL
// mapping is src/lib/docs-version.mjs, where it is tested.
import { proxiedLocation, proxyTarget } from '../src/lib/docs-version.mjs';

/** @param {{ request: Request, env: Record<string, string>, next: () => Promise<Response> }} context */
export async function proxy({ request, env, next }) {
    const url = new URL(request.url);
    const project = env.PAGES_PROJECT || undefined;
    const target = proxyTarget(url.pathname, project);
    if (target === null) return next();
    const upstream = await fetch(`${target}${url.search}`, {
        method: request.method,
        headers: request.headers,
        redirect: 'manual',
    });
    const location = upstream.headers.get('location');
    if (location === null) return upstream;
    const headers = new Headers(upstream.headers);
    const absolute = new URL(location, target).toString();
    const rewritten = proxiedLocation(absolute, url.pathname, project);
    if (rewritten === null) return upstream;
    headers.set('location', rewritten);
    return new Response(upstream.body, { status: upstream.status, headers });
}
