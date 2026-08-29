import { useQuery } from '@tanstack/react-query';

/**
 * The loading placeholder is a plain skeleton.
 *
 * The interim JS placeholder decode this component used to run was removed with the move to
 * Chromahash (design/thumbnails — LQIP): there is deliberately no JS placeholder codec. The
 * browser also has nothing to decode today — the authenticated read path is a key-free
 * projection of the sync feed, so the encrypted `lqip` never reaches it. When it does, it
 * decodes through `capsule-wasm` over the same `capsule-core::lqip` code the native clients
 * use, never a second implementation in JS.
 */
export function LazyImage({
    src,
    alt,
    className,
    ...props
}: React.ImgHTMLAttributes<HTMLImageElement>) {
    const { data: loadedSrc, isSuccess } = useQuery({
        queryKey: ['image', src],
        queryFn: async () => {
            // Artificial delay to simulate network latency as requested
            await new Promise((r) => setTimeout(r, 100 + Math.random() * 150));

            if (!src) throw new Error('No src');

            return new Promise<string>((resolve, reject) => {
                const img = new Image();
                img.src = src;
                img.onload = () => resolve(src);
                img.onerror = reject;
            });
        },
        staleTime: Number.POSITIVE_INFINITY,
        enabled: !!src,
        // We don't retry immediately for images to avoid flickering if failed
        retry: 1,
    });

    const isLoaded = isSuccess && loadedSrc;

    return (
        <div
            className={`relative overflow-hidden w-full h-full bg-muted ${className || ''}`}
        >
            <img
                src={loadedSrc || ''}
                data-loaded={!!isLoaded}
                className={`w-full h-full object-cover transition-opacity duration-500 ease-in-out ${isLoaded ? 'opacity-100' : 'opacity-0'}`}
                {...props}
                alt={alt ?? ''}
            />

            {!isLoaded && (
                <div className="absolute inset-0 pointer-events-none">
                    <div className="w-full h-full bg-muted animate-pulse" />
                </div>
            )}
        </div>
    );
}
