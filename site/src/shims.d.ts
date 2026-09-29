declare module "../dist/worker.js" {
  const worker: {
    fetch(request: Request, env?: unknown): Promise<Response>;
  };
  export default worker;
}

declare module "*/dist/worker.js" {
  const worker: {
    fetch(request: Request, env?: unknown): Promise<Response>;
  };
  export default worker;
}
