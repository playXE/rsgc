package main

import (
	"fmt"
	"os"
	"strconv"
	"sync"
	"time"
)

type TreeNode struct {
	left, right *TreeNode
}

func bottomUpTree(depth int) *TreeNode {
	if 0 < depth {
		return &TreeNode{bottomUpTree(depth - 1), bottomUpTree(depth - 1)}
	} else {
		return &TreeNode{nil, nil}
	}
}

func (node *TreeNode) itemCheck() int {
	if node.left == nil {
		return 1
	} else {
		return 1 + node.left.itemCheck() + node.right.itemCheck()
	}
}

func loops(ch chan string, max_depth int, min_depth int, depth int) {
	check := 0

	iterations := 1 << (max_depth - depth + min_depth)

	for i := 1; i <= iterations; i++ {
		check += bottomUpTree(depth).itemCheck()
	}

	ch <- fmt.Sprintf("%d\t trees of depth %d\t check: %d\n", iterations, depth, check)
}

func main() {
	var wg sync.WaitGroup
	n := 0
	if len(os.Args) > 1 {
		n, _ = strconv.Atoi(os.Args[1])
	}
	startTime := time.Now()
	minDepth := 4
	maxDepth := 0
	if n < minDepth+2 {
		maxDepth = minDepth + 2
	} else {
		maxDepth = n
	}
	stretchDepth := maxDepth + 1

	check := bottomUpTree(stretchDepth).itemCheck()
	fmt.Printf("stretch tree of depth %d\t check: %d\n", stretchDepth, check)

	longLivedTree := bottomUpTree(maxDepth)
	results := make([]string, (maxDepth-minDepth)/2+1, (maxDepth-minDepth)/2+1)
	for depth := minDepth; depth <= maxDepth; depth += 2 {
		wg.Add(1)
		go func(depth int, minDepth int, maxDepth int) {
			iterations := 1 << (maxDepth - depth + minDepth)
			check := 0
			for i := 1; i <= iterations; i++ {
				check += bottomUpTree(depth).itemCheck()
			}

			results[(depth-minDepth)/2] = fmt.Sprintf("%d\t trees of depth %d\t check: %d\n", iterations, depth, check)
			wg.Done()
		}(depth, minDepth, maxDepth)
	}

	wg.Wait()

	for i := 0; i < len(results); i++ {
		fmt.Print(results[i])
	}

	fmt.Printf("long lived tree of depth %d\t check: %d\n", maxDepth, longLivedTree.itemCheck())

	endTime := time.Now()
	elapsed := endTime.Sub(startTime)

	fmt.Printf("time: %dms\n", elapsed.Milliseconds())
}
