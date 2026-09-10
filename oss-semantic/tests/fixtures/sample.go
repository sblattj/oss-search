package geom

// Point is a 2D point.
type Point struct {
    X, Y float64
}

// Dist returns the euclidean norm.
func Dist(p Point) float64 {
    return math.Hypot(p.X, p.Y)
}

func (p Point) Scale(f float64) Point {
    return Point{p.X * f, p.Y * f}
}
