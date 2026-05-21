import { Checkbox } from '@/components/ui/checkbox';
import {
    ContextMenu,
    ContextMenuContent,
    ContextMenuItem,
    ContextMenuSeparator,
    ContextMenuSub,
    ContextMenuSubContent,
    ContextMenuSubTrigger,
    ContextMenuTrigger,
} from '@/components/ui/context-menu';
import { type Asset, mockAlbums } from '@/lib/mock-data';
import { useVirtualizer } from '@tanstack/react-virtual';
import { Check, FolderInput, PlayCircle, Share, Trash2 } from 'lucide-react';
import { useEffect, useMemo, useRef, useState } from 'react';
import { toast } from 'sonner';
import { LazyImage } from './lazy-image';

interface AssetGridProps {
    assets: Asset[];
    onAssetClick?: (asset: Asset) => void;
}

const GAP = 12; // Gap between items
const TARGET_ROW_HEIGHT = 200; // Target height for rows

/**
 * Hook to track element dimensions
 */
function useElementSize<T extends HTMLElement>() {
    const ref = useRef<T>(null);
    const [size, setSize] = useState({ width: 0, height: 0 });

    useEffect(() => {
        if (!ref.current) return;

        const observer = new ResizeObserver((entries) => {
            const entry = entries[0];
            if (entry) {
                // Use contentRect for accurate content box dimensions
                const { width, height } = entry.contentRect;
                setSize({ width, height });
            }
        });

        observer.observe(ref.current);
        return () => observer.disconnect();
    }, []);

    return { ref, size };
}

interface RowData {
    items: {
        asset: Asset;
        width: number;
        height: number;
    }[];
    height: number;
    startIndex: number;
}

export const AssetGrid = ({ assets, onAssetClick }: AssetGridProps) => {
    const { ref: containerRef, size } = useElementSize<HTMLDivElement>();

    // Selection State
    const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set());
    const [lastClickedId, setLastClickedId] = useState<string | null>(null);

    const handleAssetClick = (asset: Asset, e: React.MouseEvent) => {
        // Prevent default browser behavior if needed
        // e.preventDefault(); // Sometimes needed but check conflict with ContextMenu

        if (e.metaKey || e.ctrlKey) {
            // Toggle selection
            const newSelected = new Set(selectedIds);
            if (newSelected.has(asset.id)) {
                newSelected.delete(asset.id);
            } else {
                newSelected.add(asset.id);
            }
            setSelectedIds(newSelected);
            setLastClickedId(asset.id);
        } else if (e.shiftKey && lastClickedId) {
            // Range selection
            const currentIndex = assets.findIndex((a) => a.id === asset.id);
            const lastIndex = assets.findIndex((a) => a.id === lastClickedId);

            if (currentIndex !== -1 && lastIndex !== -1) {
                const start = Math.min(currentIndex, lastIndex);
                const end = Math.max(currentIndex, lastIndex);

                const newSelected = new Set(selectedIds);
                for (let i = start; i <= end; i++) {
                    newSelected.add(assets[i].id);
                }
                setSelectedIds(newSelected);
                // Don't update lastClickedId on shift click usually, or do?
                // Standard behavior: lastClicked remains anchor
            }
        } else {
            // Default click behavior
            if (selectedIds.size > 0) {
                // If selection is active, simple click clears it (optional standard behavior)
                // Or we navigate.
                // let's navigate if no keys pressed
                onAssetClick?.(asset);
            } else {
                onAssetClick?.(asset);
            }
        }
    };

    const toggleSelection = (assetId: string) => {
        const newSelected = new Set(selectedIds);
        if (newSelected.has(assetId)) {
            newSelected.delete(assetId);
        } else {
            newSelected.add(assetId);
        }
        setSelectedIds(newSelected);
        setLastClickedId(assetId);
    };

    const handleContextMenu = (asset: Asset, e: React.MouseEvent) => {
        // If right clicking an item that is NOT selected, select it (and deselect others)
        // mimics standard OS behavior
        if (!selectedIds.has(asset.id)) {
            setSelectedIds(new Set([asset.id]));
            setLastClickedId(asset.id);
        }
    };

    // Actions
    const handleMoveToAlbum = (albumTitle: string) => {
        toast.success(`Moved ${selectedIds.size} items to ${albumTitle}`);
        setSelectedIds(new Set());
    };

    const handleDelete = () => {
        toast.success(`Deleted ${selectedIds.size} items`);
        setSelectedIds(new Set());
    };

    // Linear Partition / Greedy Row Layout
    const rows = useMemo(() => {
        if (!size.width) return [];

        const result: RowData[] = [];
        let currentRowItems: Asset[] = [];
        let currentAspectRatioSum = 0;
        let startIndex = 0;

        for (let i = 0; i < assets.length; i++) {
            const asset = assets[i];
            const aspectRatio = asset.width / asset.height;

            currentRowItems.push(asset);
            currentAspectRatioSum += aspectRatio;

            const gaps = (currentRowItems.length - 1) * GAP;
            const potentialHeight = (size.width - gaps) / currentAspectRatioSum;

            if (potentialHeight < TARGET_ROW_HEIGHT) {
                if (currentRowItems.length > 1) {
                    const prevAspectRatioSum =
                        currentAspectRatioSum - aspectRatio;
                    const prevGaps = (currentRowItems.length - 2) * GAP;
                    const prevHeight =
                        (size.width - prevGaps) / prevAspectRatioSum;

                    if (
                        Math.abs(prevHeight - TARGET_ROW_HEIGHT) <
                        Math.abs(potentialHeight - TARGET_ROW_HEIGHT)
                    ) {
                        currentRowItems.pop();
                        currentAspectRatioSum -= aspectRatio;
                        i--;
                    }
                }

                const finalGaps = (currentRowItems.length - 1) * GAP;
                const finalHeight =
                    (size.width - finalGaps) / currentAspectRatioSum;

                result.push({
                    items: currentRowItems.map((a) => ({
                        asset: a,
                        width: finalHeight * (a.width / a.height),
                        height: finalHeight,
                    })),
                    height: finalHeight,
                    startIndex,
                });

                startIndex += currentRowItems.length;
                currentRowItems = [];
                currentAspectRatioSum = 0;
            }
        }

        if (currentRowItems.length > 0) {
            const finalHeight = TARGET_ROW_HEIGHT;
            result.push({
                items: currentRowItems.map((a) => ({
                    asset: a,
                    width: finalHeight * (a.width / a.height),
                    height: finalHeight,
                })),
                height: finalHeight,
                startIndex,
            });
        }

        return result;
    }, [assets, size.width]);

    return (
        <div
            ref={containerRef}
            className="h-full w-full overflow-y-auto p-4 scroll-smooth"
            // Clear selection on background click?
            // onClick={(e) => { if(e.target === e.currentTarget) setSelectedIds(new Set()) }}
        >
            <div className="relative min-h-full">
                <DateIndicator containerRef={containerRef} rows={rows} />

                {size.width > 0 && (
                    <VirtualList
                        rows={rows}
                        containerRef={containerRef}
                        // Props for Item
                        selectedIds={selectedIds}
                        onAssetClick={handleAssetClick}
                        onToggleSelection={toggleSelection}
                        onContextMenu={handleContextMenu}
                        onMoveToAlbum={handleMoveToAlbum}
                        onDelete={handleDelete}
                    />
                )}

                {assets.length === 0 && (
                    <div className="absolute inset-0 flex items-center justify-center text-muted-foreground">
                        No photos found
                    </div>
                )}
            </div>
        </div>
    );
};

interface VirtualListProps {
    rows: RowData[];
    containerRef: React.RefObject<HTMLDivElement | null>;
    selectedIds: Set<string>;
    onAssetClick: (asset: Asset, e: React.MouseEvent) => void;
    onToggleSelection: (id: string) => void;
    onContextMenu: (asset: Asset, e: React.MouseEvent) => void;
    onMoveToAlbum: (title: string) => void;
    onDelete: () => void;
}

const VirtualList = ({
    rows,
    containerRef,
    selectedIds,
    onAssetClick,
    onToggleSelection,
    onContextMenu,
    onMoveToAlbum,
    onDelete,
}: VirtualListProps) => {
    const rowVirtualizer = useVirtualizer({
        count: rows.length,
        getScrollElement: () => containerRef.current,
        estimateSize: (index) => rows[index].height + GAP,
        overscan: 5,
    });

    return (
        <div
            className="relative w-full"
            style={{
                height: `${rowVirtualizer.getTotalSize()}px`,
            }}
        >
            {rowVirtualizer.getVirtualItems().map((virtualRow) => {
                const row = rows[virtualRow.index];
                return (
                    <div
                        key={virtualRow.index}
                        className="absolute top-0 left-0 flex w-full"
                        style={{
                            height: `${row.height}px`,
                            transform: `translateY(${virtualRow.start}px)`,
                            gap: GAP,
                        }}
                    >
                        {row.items.map((item) => (
                            <div
                                key={item.asset.id}
                                style={{ width: item.width }}
                                className="h-full shrink-0"
                            >
                                <AssetCard
                                    asset={item.asset}
                                    selected={selectedIds.has(item.asset.id)}
                                    onClick={(e) => onAssetClick(item.asset, e)}
                                    onToggle={() =>
                                        onToggleSelection(item.asset.id)
                                    }
                                    onContextMenu={(e) =>
                                        onContextMenu(item.asset, e)
                                    }
                                    onMoveToAlbum={onMoveToAlbum}
                                    onDelete={onDelete}
                                />
                            </div>
                        ))}
                    </div>
                );
            })}
        </div>
    );
};

interface AssetCardProps {
    asset: Asset;
    selected: boolean;
    onClick: (e: React.MouseEvent) => void;
    onToggle: () => void;
    onContextMenu: (e: React.MouseEvent) => void;
    onMoveToAlbum: (title: string) => void;
    onDelete: () => void;
}

const AssetCard = ({
    asset,
    selected,
    onClick,
    onToggle,
    onContextMenu,
    onMoveToAlbum,
    onDelete,
}: AssetCardProps) => {
    return (
        <ContextMenu>
            <ContextMenuTrigger asChild>
                <div
                    className={`group relative w-full h-full bg-muted rounded-md overflow-hidden cursor-pointer hover:shadow-lg transition-all
                        ${selected ? 'ring-2 ring-primary ring-offset-2' : ''}
                    `}
                    onKeyUp={onClick}
                    onContextMenu={onContextMenu}
                >
                    <LazyImage
                        src={asset.url}
                        thumbhash={asset.thumbhash}
                        alt="Asset"
                        className={`transition-transform duration-300 group-hover:scale-105 ${selected ? 'scale-105' : ''}`}
                    />

                    {/* Dark overlay on hover or when selected to make icons visible */}
                    <div
                        className={`absolute inset-0 bg-black/0 transition-colors duration-200
                        ${selected ? 'bg-black/10' : 'group-hover:bg-black/10'}
                    `}
                    />

                    {/* Selection Checkbox - Visible on hover or if selected */}
                    <div
                        className={`absolute top-2 left-2 transition-opacity duration-200 z-10
                            ${selected ? 'opacity-100' : 'opacity-0 group-hover:opacity-100'}
                        `}
                        onKeyUp={(e) => {
                            e.stopPropagation(); // Don't trigger main click
                            onToggle();
                        }}
                    >
                        <div
                            className={`w-5 h-5 rounded-full border-2 flex items-center justify-center
                            ${selected ? 'bg-primary border-primary' : 'bg-black/20 border-white hover:bg-black/40'}
                        `}
                        >
                            {selected && (
                                <Check className="w-3 h-3 text-primary-foreground" />
                            )}
                        </div>
                    </div>

                    {asset.type === 'video' && (
                        <div className="absolute top-1/2 left-1/2 -translate-x-1/2 -translate-y-1/2 text-white drop-shadow-md pointer-events-none">
                            <PlayCircle className="w-8 h-8 fill-black/20" />
                        </div>
                    )}

                    {asset.duration && (
                        <div className="absolute bottom-2 right-2 text-[10px] font-medium text-white bg-black/60 px-1.5 py-0.5 rounded-sm pointer-events-none">
                            {asset.duration}
                        </div>
                    )}
                </div>
            </ContextMenuTrigger>
            <ContextMenuContent className="w-52">
                <ContextMenuSub>
                    <ContextMenuSubTrigger>
                        <FolderInput className="w-4 h-4 mr-2" />
                        Move to Album
                    </ContextMenuSubTrigger>
                    <ContextMenuSubContent className="w-48">
                        {mockAlbums.map((album) => (
                            <ContextMenuItem
                                key={album.id}
                                onClick={() => onMoveToAlbum(album.title)}
                            >
                                {album.title}
                            </ContextMenuItem>
                        ))}
                    </ContextMenuSubContent>
                </ContextMenuSub>
                <ContextMenuSeparator />
                <ContextMenuItem>
                    <Share className="w-4 h-4 mr-2" />
                    Share
                </ContextMenuItem>
                <ContextMenuSeparator />
                <ContextMenuItem
                    className="text-destructive focus:text-destructive"
                    onClick={onDelete}
                >
                    <Trash2 className="w-4 h-4 mr-2" />
                    Delete
                </ContextMenuItem>
            </ContextMenuContent>
        </ContextMenu>
    );
};

// Date Indicator component remains same as previous...
const DateIndicator = ({
    containerRef,
    rows,
}: {
    containerRef: React.RefObject<HTMLDivElement | null>;
    rows: RowData[];
}) => {
    const [dateLabel, setDateLabel] = useState<string | null>(null);

    useEffect(() => {
        const container = containerRef.current;
        if (!container) return;

        const index: { y: number; date: Date }[] = [];
        let y = 0;
        for (const row of rows) {
            if (row.items.length > 0) {
                index.push({ y, date: row.items[0].asset.date });
            }
            y += row.height + GAP;
        }

        const update = () => {
            const scrollTop = container.scrollTop + 50;
            let low = 0;
            let high = index.length - 1;
            let res = 0;
            while (low <= high) {
                const mid = Math.floor((low + high) / 2);
                if (index[mid].y < scrollTop) {
                    res = mid;
                    low = mid + 1;
                } else {
                    high = mid - 1;
                }
            }

            if (index[res]) {
                const d = index[res].date;
                const label = d.toLocaleDateString('en-US', {
                    month: 'short',
                    year: 'numeric',
                });
                setDateLabel(label);
            }
        };

        container.addEventListener('scroll', update);
        update();
        return () => container.removeEventListener('scroll', update);
    }, [containerRef, rows]);

    if (!dateLabel) return null;

    return (
        <div className="sticky top-4 z-50 flex justify-center pointer-events-none">
            <div className="bg-black/60 backdrop-blur-md text-white px-3 py-1 rounded-full text-sm font-medium shadow-lg transition-opacity duration-300">
                {dateLabel}
            </div>
        </div>
    );
};
