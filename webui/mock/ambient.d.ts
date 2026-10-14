/** Bun server declarations for `bun run typecheck:mock`; browser and Node types
 * come from the shared tsconfig, including the actual locale catalogs. */
interface BunServeOptions {
  port?: number;
  hostname?: string;
  fetch(request: Request): Response | Promise<Response>;
}

interface BunServer {
  readonly port: number;
  readonly hostname: string;
  stop(closeActiveConnections?: boolean): void;
}

declare const Bun: {
  serve(options: BunServeOptions): BunServer;
};
