package calc

import (
	"testing"
	"time"
)

func TestAdds(t *testing.T) {
	if Add(2, 3) != 5 {
		t.Fatal("2+3")
	}
}

func TestAddsAgain(t *testing.T) {
	if Add(2, 3) != 5 {
		t.Fatal("2+3")
	}
}

func TestAdult(t *testing.T) {
	if !IsAdult(30) {
		t.Fatal("30")
	}
}

func TestSlow(t *testing.T) {
	time.Sleep(600 * time.Millisecond)
	if Add(1, 1) != 2 {
		t.Fatal("1+1")
	}
}
