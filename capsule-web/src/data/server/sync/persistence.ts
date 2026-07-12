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
const DB_VERSION = 1;

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
                if (!request.result.objectStoreNames.contains(STORE_NAME)) {
                    request.result.createObjectStore(STORE_NAME);
                }
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
