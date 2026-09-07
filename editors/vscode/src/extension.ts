import * as path from 'path';
import * as vscode from 'vscode';
import {
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
} from 'vscode-languageclient/node';

let client: LanguageClient | undefined;

export function activate(context: vscode.ExtensionContext): void {
  const serverPath = resolveServerPath(context);

  const serverOptions: ServerOptions = {
    command: serverPath,
    args: [],
  };

  const clientOptions: LanguageClientOptions = {
    documentSelector: [{ scheme: 'file', language: 'hulk' }],
  };

  client = new LanguageClient(
    'hulk',
    'HULK Language Server',
    serverOptions,
    clientOptions
  );
  context.subscriptions.push({ dispose: () => client?.stop() });
  client.start();
}

export function deactivate(): Thenable<void> | undefined {
  return client?.stop();
}

/// Resolves the `hulk-lsp` binary path: the `hulk.serverPath` setting if
/// set, otherwise the development build at `target/debug/hulk-lsp(.exe)`
/// relative to the repository root (this extension lives at
/// `editors/vscode/`, so the repository root is two levels up).
function resolveServerPath(context: vscode.ExtensionContext): string {
  const configured = vscode.workspace
    .getConfiguration('hulk')
    .get<string>('serverPath');
  if (configured) {
    return configured;
  }
  const exeName = process.platform === 'win32' ? 'hulk-lsp.exe' : 'hulk-lsp';
  return context.asAbsolutePath(path.join('..', '..', 'target', 'debug', exeName));
}
