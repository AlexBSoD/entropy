// Compile before credentials are available. Import the existing encrypted P12
// directly from memory: no certificate/password file and no trust changes.
import Foundation
import Security

func requireSuccess(_ status: OSStatus) {
    guard status == errSecSuccess else {
        // Never dump framework errors or the environment containing credentials.
        fputs("Certificate import failed\n", stderr)
        exit(1)
    }
}

let environment = ProcessInfo.processInfo.environment
guard CommandLine.arguments.count == 2,
      let encoded = environment["MACOS_CERTIFICATE_P12_BASE64"],
      let password = environment["MACOS_CERTIFICATE_PASSWORD"],
      !encoded.isEmpty, !password.isEmpty,
      let data = Data(base64Encoded: encoded) else {
    fputs("Missing or invalid signing credentials\n", stderr)
    exit(1)
}
var keychain: SecKeychain?
requireSuccess(SecKeychainOpen(CommandLine.arguments[1], &keychain))
var application: SecTrustedApplication?
requireSuccess(SecTrustedApplicationCreateFromPath("/usr/bin/codesign", &application))
var access: SecAccess?
requireSuccess(SecAccessCreate("Entropy release signing" as CFString,
                               [application!] as CFArray, &access))
let options: [String: Any] = [
    kSecImportExportPassphrase as String: password,
    kSecImportExportKeychain as String: keychain!,
    kSecImportExportAccess as String: access!
]
var items: CFArray?
requireSuccess(SecPKCS12Import(data as CFData, options as CFDictionary, &items))
