/// The size limits of the file pane, decided once for every surface that offers a file.
public enum FileLimits {
    /// Files above this size are never offered for display, even with "Load anyway".
    public static let hardLimit: UInt64 = 512 * 1024 * 1024

    /// Whether a file of `size` bytes, rejected by the default limit, may be loaded anyway.
    public static func canForceLoad(size: UInt64) -> Bool {
        size <= hardLimit
    }
}
