import { Button } from '@/components/ui/button';
import { Link, createLazyFileRoute } from '@tanstack/react-router';

export const Route = createLazyFileRoute('/')({
    component: Index,
});

function Index() {
    return (
        <div className="flex flex-col items-center justify-center min-h-[50vh] gap-4">
            <h1 className="text-4xl font-bold">Welcome to Capsule</h1>
            <p className="text-muted-foreground">
                Your professional asset management solution.
            </p>
            <div className="flex gap-4">
                <Link to="/dashboard">
                    <Button>Go to Dashboard</Button>
                </Link>
                <Link to="/photos">
                    <Button variant="outline">View Photos</Button>
                </Link>
            </div>
        </div>
    );
}
