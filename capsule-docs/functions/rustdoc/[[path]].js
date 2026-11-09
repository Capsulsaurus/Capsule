// /rustdoc/* → the workspace rustdoc on GitHub Pages. See functions/_proxy.mjs.
import { proxy } from '../_proxy.mjs';

export const onRequest = proxy;
