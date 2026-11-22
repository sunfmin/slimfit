package calc

func Add(a, b int) int {
	return a + b
}

func Unused(x int) int {
	doubled := x * 2
	shifted := doubled + 7
	return shifted / 3
}

func IsAdult(age int) bool {
	return age >= 18
}

func Negate(x int) int {
	return -x
}

func Triple(x int) int {
	return x * 3
}
