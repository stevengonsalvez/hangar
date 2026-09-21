import XCTest

final class FleetFixtureDaemonFallbackTests: XCTestCase {
    func testLocalFallbackResolvesFixtureWithoutEnvironmentOverride() throws {
        let executable = FleetFixtureDaemon.fixtureExecutableURL(sourceFilePath: #filePath, environment: [:])

        // The repository root, derived here independently of the code under
        // test: this file sits five components below it
        // (apps/ainb-fleet-macos/Tests/FleetRPCTests/<this file>).
        var repository = URL(fileURLWithPath: #filePath)
        for _ in 0..<5 { repository.deleteLastPathComponent() }
        XCTAssertTrue(
            FileManager.default.fileExists(atPath: repository.appendingPathComponent("crates").path),
            "the five-component walk did not land on the repository root"
        )

        // The FULL path, not a suffix. Every wrong answer this fallback can
        // give ends in the same suffix: the degenerate branch returns
        // <this directory>/target/debug/examples/fleet_fixture_daemon, and an
        // environment override can name any root at all. Only the whole path
        // says WHICH root the upward search settled on.
        XCTAssertEqual(
            executable.standardizedFileURL.path,
            repository
                .appendingPathComponent("target/debug/examples/fleet_fixture_daemon")
                .standardizedFileURL.path
        )
    }
}
