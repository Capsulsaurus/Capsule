// The proxy both Pages Functions share: forward the request to the origin
// `proxyTarget` names (an old version's branch deployment, or GitHub Pages for
// rustdoc), and keep any redirect it answers with on the docs domain. The URL
// mapping is src/lib/docs-version.mjs, where it is tested.
import {
    proxiedLocation,
    proxyTarget,
    slashRedirect,
} from '../src/lib/docs-version.mjs';

// Only what a static origin needs for content negotiation and caching: the
// docs domain's cookies and credentials never leave it.
const FORWARDED = [
    'accept',
    'accept-encoding',
    'accept-language',
    'if-modified-since',
    'if-none-match',
    'range',
];

/** @param {{ request: Request, env: Record<string, string>, next: () => Promise<Response> }} context */
export async function proxy({ request, env, next }) {
    const url = new URL(request.url);
    // `/rustdoc` and `/v/X.Y` resolve their relative links against `/`, so
    // they are sent to the slashed path first.
    const slashed = slashRedirect(url.pathname);
    if (slashed !== null) {
        return Response.redirect(new URL(`${slashed}${url.search}`, url), 301);
    }
    const project = env.PAGES_PROJECT || undefined;
    const target = proxyTarget(url.pathname, project);
    if (target === null) return next();
    if (request.method !== 'GET' && request.method !== 'HEAD') {
        return new Response(null, {
            status: 405,
            headers: { allow: 'GET, HEAD' },
        });
    }
    const headers = new Headers();
    for (const name of FORWARDED) {
        const value = request.headers.get(name);
        if (value !== null) headers.set(name, value);
    }
    const upstream = await fetch(`${target}${url.search}`, {
        method: request.method,
        headers,
        redirect: 'manual',
    });
    const location = upstream.headers.get('location');
    if (location === null) return upstream;
    const rewritten = proxiedLocation(
        new URL(location, target).toString(),
        url.pathname,
        project,
    );
    if (rewritten === null) return upstream;
    const response = new Headers(upstream.headers);
    response.set('location', rewritten);
    return new Response(upstream.body, {
        status: upstream.status,
        headers: response,
    });
}
