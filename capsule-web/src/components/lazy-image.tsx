import { useQuery } from '@tanstack/react-query';
type LazyImageProps = React.ImgHTMLAttributes<HTMLImageElement>;

export function LazyImage({ src, alt, className, ...props }: LazyImageProps) {
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
