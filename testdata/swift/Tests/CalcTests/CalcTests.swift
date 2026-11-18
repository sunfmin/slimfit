import XCTest
@testable import Calc

final class CalcTests: XCTestCase {
    func testAdds() {
        XCTAssertEqual(Calc.add(2, 3), 5)
    }

    func testAddsAgain() {
        XCTAssertEqual(Calc.add(2, 3), 5)
    }

    func testIsAdult() {
        XCTAssertTrue(isAdult(18))
    }

    func testSlow() {
        Thread.sleep(forTimeInterval: 0.6)
        XCTAssertEqual(Calc.add(1, 1), 2)
    }
}
