import { createLazyFileRoute } from '@tanstack/react-router';

export const Route = createLazyFileRoute('/library/favorites')({
    component: RouteComponent,
});

function RouteComponent() {
    return <div>Hello "/library/favorites"!</div>;
}
