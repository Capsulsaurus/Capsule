/**
 * Persistence for the sync snapshot (cursor + high-water + records). IndexedDB is the
 * durable choice for the `library.sqlite` analogue — it holds the whole record set, not
 * just the cursor, and survives reloads. The seam keeps `SyncStore`/`ServerGateway`
 * unit-testable with an in-memory adapter (bun tests have no IndexedDB).
 */

import type { SyncSnapshot } from './store';

/** Load/save the whole sync snapshot atomically (one row keyed by the library id). */
export interface SyncPersistence {
    load(): Promise<SyncSnapshot | null>;
    save(snapshot: SyncSnapshot): Promise<void>;
}

const DB_NAME = 'capsule-sync';
const STORE_NAME = 'snapshot';
/**
 * Bumped to 2 by `S-C59`, and the bump is load-bearing rather than cosmetic.
 *
 * A version-1 snapshot holds a cursor from the retired gRPC feed: base64 of the cursor *bytes*.
 * The REST feed's cursor is a MAC'd string, so a v1 cursor replayed against it is refused —
 * forever, and **silently**, because `ServerGateway` treats a transport error as "offline" and
 * warns rather than throwing. A browser that had synced once would simply stop receiving
 * changes with no visible failure.
 *
 * So the upgrade **drops** the store rather than migrating it. There is nothing to migrate: the
 * snapshot is a cache of a feed that is the source of truth, and re-syncing from the beginning
 * costs one catch-up. That is the one case where discarding client state is right — it holds no
 * user data that is not re-derivable, which is exactly what `versioning.md`'s client-catalog
 * clause distinguishes from the local library.
 */
const DB_VERSION = 2;

/**
 * IndexedDB-backed persistence. The connection opens lazily on first use, so constructing
 * this adapter touches no browser globals (safe to build/import outside a live DOM).
 */
export class IndexedDbPersistence implements SyncPersistence {
    private db: Promise<IDBDatabase> | null = null;

    /** @param key snapshot row key — one per logical library (defaults to `default`). */
    constructor(private readonly key: string = 'default') {}

    private open(): Promise<IDBDatabase> {
        if (this.db) {
            return this.db;
        }
        this.db = new Promise<IDBDatabase>((resolve, reject) => {
            const request = indexedDB.open(DB_NAME, DB_VERSION);
            request.onupgradeneeded = () => {
                // Drop and recreate. See `DB_VERSION`: a snapshot from an older schema carries
                // a cursor the current feed cannot honour, and keeping it would wedge the sync
                // silently rather than costing one catch-up.
                if (request.result.objectStoreNames.contains(STORE_NAME)) {
                    request.result.deleteObjectStore(STORE_NAME);
                }
                request.result.createObjectStore(STORE_NAME);
            };
            request.onsuccess = () => resolve(request.result);
            request.onerror = () => reject(request.error);
        });
        return this.db;
    }

    async load(): Promise<SyncSnapshot | null> {
        const db = await this.open();
        return new Promise<SyncSnapshot | null>((resolve, reject) => {
            const tx = db.transaction(STORE_NAME, 'readonly');
            const request = tx.objectStore(STORE_NAME).get(this.key);
            request.onsuccess = () =>
                resolve((request.result as SyncSnapshot | undefined) ?? null);
            request.onerror = () => reject(request.error);
        });
    }

    async save(snapshot: SyncSnapshot): Promise<void> {
        const db = await this.open();
        return new Promise<void>((resolve, reject) => {
            const tx = db.transaction(STORE_NAME, 'readwrite');
            tx.objectStore(STORE_NAME).put(snapshot, this.key);
            tx.oncomplete = () => resolve();
            tx.onerror = () => reject(tx.error);
        });
    }
}

/** In-memory persistence for tests and non-persistent environments. */
export class MemoryPersistence implements SyncPersistence {
    private snapshot: SyncSnapshot | null = null;

    load(): Promise<SyncSnapshot | null> {
        return Promise.resolve(this.snapshot);
    }

    save(snapshot: SyncSnapshot): Promise<void> {
        this.snapshot = snapshot;
        return Promise.resolve();
    }
}
