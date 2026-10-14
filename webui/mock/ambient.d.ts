/**
 * Ambient declarations for the standalone mock-server typecheck:
 *
 *   bunx tsc --noEmit --strict --skipLibCheck --module esnext \
 *     --moduleResolution bundler --target esnext mock/
 *
 * That invocation loads no webui tsconfig (no `paths`, no DOM lib), but the
 * type-only imports from `src/lib` pull `actor-api.ts` -> `i18n.ts` ->
 * `preferences.ts` into the program, which reference browser globals and the
 * `@/locales/*.json` alias. `@types/node` covers fetch/streams/crypto; this
 * file covers the rest. At runtime Bun provides the real `Bun` global and the
 * browser-globals-only code is never executed by the mock.
 */

declare module "@/locales/en-US.json" {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const catalog: any;
  export default catalog;
}

declare module "@/locales/zh-CN.json" {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const catalog: any;
  export default catalog;
}

interface MockClassList {
  toggle(token: string, force?: boolean): boolean;
}

declare const document:
  | {
      documentElement: {
        lang: string;
        classList: MockClassList;
        style: { colorScheme: string };
      };
    }
  | undefined;

declare const localStorage:
  | {
      getItem(key: string): string | null;
      setItem(key: string, value: string): void;
      removeItem(key: string): void;
    }
  | undefined;

declare const matchMedia: ((query: string) => { matches: boolean }) | undefined;

interface BunServeOptions {
  port?: number;
  hostname?: string;
  fetch?(request: Request): Response | Promise<Response>;
}

interface BunServer {
  readonly port: number;
  readonly hostname: string;
  stop(closeActiveConnections?: boolean): void;
}

declare const process: { env: Record<string, string | undefined> };

declare const Bun: {
  serve(options: BunServeOptions): BunServer;
};
