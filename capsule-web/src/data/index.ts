import type { CapsuleGateway } from './gateway';
import { mockGateway } from './mock/mock-gateway';

// The real adapter lands here when the server schema is live:
//   import { serverGateway } from './server/server-gateway';
//   export const gateway = PUBLIC_DATA_SOURCE === 'server' ? serverGateway : mockGateway;

/** The active data source the UI reads through (mock until a server exists). */
export const gateway: CapsuleGateway = mockGateway;
