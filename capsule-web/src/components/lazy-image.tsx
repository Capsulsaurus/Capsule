import { useQuery } from '@tanstack/react-query';
import { useMemo } from 'react';
import { thumbHashToRGBA } from 'thumbhash';

// Helper to decode base64 to byte array
function base64ToBytes(base64: string) {
    const binary = atob(base64);
    const bytes = new Uint8Array(binary.length);
    for (let i = 0; i < binary.length; i++) {
        bytes[i] = binary.charCodeAt(i);
    }
    return bytes;
}

// Convert rgba to data url
function thumbHashToDataURL(hash: string) {
    const bytes = base64ToBytes(hash);
    const { w, h, rgba } = thumbHashToRGBA(bytes);
    const canvas = document.createElement('canvas');
    canvas.width = w;
    canvas.height = h;
    const ctx = canvas.getContext('2d');
    if (!ctx) return '';
    const imageData = ctx.createImageData(w, h);
    imageData.data.set(rgba);
    ctx.putImageData(imageData, 0, 0);
    return canvas.toDataURL();
}

interface LazyImageProps extends React.ImgHTMLAttributes<HTMLImageElement> {
    thumbhash?: string;
}

export function LazyImage({
    src,
    alt,
    className,
    thumbhash,
    ...props
}: LazyImageProps) {
    // Generate placeholder URL
    const placeholderUrl = useMemo(() => {
        if (!thumbhash) return null;
        try {
            return thumbHashToDataURL(thumbhash);
        } catch (e) {
            console.error('Failed to decode thumbhash', e);
            return null;
        }
    }, [thumbhash]);

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
                src={loadedSrc || (placeholderUrl ?? '')}
                data-loaded={!!isLoaded}
                className={`w-full h-full object-cover transition-opacity duration-500 ease-in-out ${isLoaded ? 'opacity-100' : 'opacity-0'}`}
                {...props}
                alt={alt ?? ''}
            />

            {!isLoaded && (
                <div className="absolute inset-0 pointer-events-none">
                    {placeholderUrl ? (
                        <img
                            src={placeholderUrl}
                            alt=""
                            className="w-full h-full object-cover blur-xl scale-110 animate-pulse opacity-80"
                        />
                    ) : (
                        <div className="w-full h-full bg-muted animate-pulse" />
                    )}
                </div>
            )}
        </div>
    );
}
