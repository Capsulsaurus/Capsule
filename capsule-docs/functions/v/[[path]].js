// /v/X.Y/* → release X.Y's own deployment. See functions/_proxy.mjs.
import { proxy } from '../_proxy.mjs';

export const onRequest = proxy;
