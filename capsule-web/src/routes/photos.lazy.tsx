import { AssetGrid } from '@/components/asset-grid';
import { useAssets } from '@/data/hooks';
import { createLazyFileRoute } from '@tanstack/react-router';

export const Route = createLazyFileRoute('/photos')({
    component: Photos,
});

function Photos() {
    const { data: assets = [] } = useAssets();

    return (
        <div className="h-full flex flex-col">
            <AssetGrid
                assets={assets}
                onAssetClick={(asset) => console.info('Clicked', asset)}
            />
        </div>
    );
}
