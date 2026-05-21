import { AssetGrid } from '@/components/asset-grid';
import { mockAssets } from '@/lib/mock-data';
import { createLazyFileRoute } from '@tanstack/react-router';
import { useState } from 'react';

export const Route = createLazyFileRoute('/photos')({
    component: Photos,
});

function Photos() {
    const [assets] = useState(mockAssets);

    return (
        <div className="h-full flex flex-col">
            <AssetGrid
                assets={assets}
                onAssetClick={(asset) => console.log('Clicked', asset)}
            />
        </div>
    );
}
